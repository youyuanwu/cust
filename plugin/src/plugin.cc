// cust clang plugin — surface-extraction pass.
//
// Walks a TU's top-level decls, picks out anything carrying
// `[[clang::annotate("cust::pub")]]` (placed there by the prelude's
// cust_pub macro), and emits forward declarations into a per-
// module fragment header.
//
// Invoked by the driver as:
//
//     clang ... -fplugin=libcust_plugin.so \
//                -fplugin-arg-cust-fragment-out=<path>
//
// If `fragment-out` is absent the plugin runs but writes nothing
// — handy for clangd / IDE invocations that just want the AST
// validation.
//
// Atomicity: writes go through a `<path>.tmp` + rename so half-
// written fragments can never be observed. If the byte content
// matches what's already on disk we skip the write entirely; this
// is the cust-design.md §4 fragment-stamping invariant that lets
// downstream modules avoid rebuilds when an importee's surface
// didn't change.

#include "clang/AST/ASTConsumer.h"
#include "clang/AST/Attr.h"
#include "clang/AST/Decl.h"
#include "clang/AST/PrettyPrinter.h"
#include "clang/Basic/AttributeCommonInfo.h"
#include "clang/Basic/IdentifierTable.h"
#include "clang/Basic/LangOptions.h"
#include "clang/Basic/SourceManager.h"
#include "clang/Frontend/CompilerInstance.h"
#include "clang/Frontend/FrontendOptions.h"
#include "clang/Frontend/FrontendPluginRegistry.h"
#include "clang/Lex/Preprocessor.h"
#include "clang/Sema/ParsedAttr.h"
#include "clang/Sema/Sema.h"
#include "llvm/ADT/DenseMap.h"
#include "llvm/ADT/StringRef.h"
#include "llvm/Support/FileSystem.h"
#include "llvm/Support/MemoryBuffer.h"
#include "llvm/Support/Path.h"
#include "llvm/Support/raw_ostream.h"

#include <memory>
#include <string>
#include <system_error>
#include <vector>

using namespace clang;

namespace {

// V40D-7 attribute kind. None = decl is not pub-tagged.
// PubCrate is recognised but treated as None for fragment
// emission in slice B; slice D adds the concat-step filter
// + the /*p*/ vs /*c*/ prefix per V40D-3.
enum class CustPubKind {
    None,
    Pub,
    PubCrate,
    PubRepr,
};

// V40D-7 marker: slice E removed user-facing `annotate("cust::*")`
// source recognition. The `ParsedAttrInfo` recognisers below
// attach this sentinel alongside the actual `cust::pub` /
// `cust::test` payload; `getCustPubKind` / `getCustTestKind`
// require the sentinel to fire. A user-written
// `__attribute__((annotate("cust::pub")))` lacks the sentinel
// and is silently ignored — the only way to be recognised is
// the C23 `[[cust::*]]` spelling that goes through the
// `ParsedAttrInfo` recognisers.
constexpr llvm::StringLiteral kCustMarker = "__cust_v40_marker__";

bool hasCustMarker(const Decl *D) {
    for (const auto *attr : D->specific_attrs<AnnotateAttr>()) {
        if (attr->getAnnotation() == kCustMarker) {
            return true;
        }
    }
    return false;
}

CustPubKind getCustPubKind(const Decl *D) {
    if (!hasCustMarker(D)) {
        return CustPubKind::None;
    }
    for (const auto *attr : D->specific_attrs<AnnotateAttr>()) {
        llvm::StringRef ann = attr->getAnnotation();
        if (ann == "cust::pub") return CustPubKind::Pub;
        if (ann == "cust::pub_crate") return CustPubKind::PubCrate;
        if (ann == "cust::pub_repr") return CustPubKind::PubRepr;
    }
    return CustPubKind::None;
}

// V40D-7 + V40D-14 test attribute kind.
enum class CustTestKind {
    None,
    Test,
    TestIgnore,
};

CustTestKind getCustTestKind(const Decl *D) {
    if (!hasCustMarker(D)) {
        return CustTestKind::None;
    }
    for (const auto *attr : D->specific_attrs<AnnotateAttr>()) {
        llvm::StringRef ann = attr->getAnnotation();
        if (ann == "cust::test") return CustTestKind::Test;
        if (ann == "cust::test_ignore") return CustTestKind::TestIgnore;
    }
    return CustTestKind::None;
}

// V40D-4 error: pub_repr only applies to records/enums.
void diagPubReprOnNonRecord(DiagnosticsEngine &diags, SourceLocation loc,
                            llvm::StringRef name, llvm::StringRef kindNoun) {
    unsigned id = diags.getCustomDiagID(
        DiagnosticsEngine::Error,
        "cannot export body of `%0`: `[[cust::pub_repr]]` is only "
        "meaningful on struct, union, or enum decls (this is %1)\n"
        "  hint: drop `pub_repr` and use `[[cust::pub]]`");
    diags.Report(loc, id) << name.str() << kindNoun.str();
}

// V40D-4 enum body emission. Custom (not clang's print()) because
// V40D-4 requires every discriminant be explicit ("a future
// enum-variant reorder doesn't change the public ABI silently"),
// and clang's printer preserves explicit-vs-implicit as-written.
void renderEnumBody(const EnumDecl *ed, llvm::raw_ostream &os,
                    const PrintingPolicy &policy) {
    os << "enum " << ed->getName();
    if (ed->isFixed()) {
        os << " : ";
        ed->getIntegerType().print(os, policy);
    }
    os << " {\n";
    for (const auto *ec : ed->enumerators()) {
        os << "    " << ec->getName() << " = ";
        llvm::SmallString<32> valStr;
        ec->getInitVal().toString(valStr, /*Radix=*/10);
        os << valStr;
        os << ",\n";
    }
    os << "};\n";
}

// Render one decl into its fragment-header form. Returns the
// empty string for kinds we don't emit (anonymous records,
// unknown decl kinds). pub_repr on the wrong decl kind emits
// a diagnostic and returns empty.
std::string renderDecl(const Decl *D, CustPubKind kind, ASTContext &ctx,
                       DiagnosticsEngine &diags) {
    PrintingPolicy policy(ctx.getLangOpts());
    policy.PolishForDeclaration = true;
    policy.SuppressInitializers = true;
    policy.SuppressTagKeyword = false;

    std::string out;
    llvm::raw_string_ostream os(out);

    if (const auto *fd = dyn_cast<FunctionDecl>(D)) {
        if (kind == CustPubKind::PubRepr) {
            diagPubReprOnNonRecord(diags, fd->getLocation(),
                                   fd->getName(), "a function");
            return {};
        }
        policy.TerseOutput = true;          // no function body
        fd->print(os, policy);
        os << ";\n";
    } else if (const auto *td = dyn_cast<TypedefDecl>(D)) {
        if (kind == CustPubKind::PubRepr) {
            diagPubReprOnNonRecord(diags, td->getLocation(),
                                   td->getName(), "a typedef");
            return {};
        }
        policy.TerseOutput = true;
        // Prints as `typedef X name` — append `;` for a valid decl.
        td->print(os, policy);
        os << ";\n";
    } else if (const auto *rd = dyn_cast<RecordDecl>(D)) {
        if (!rd->getIdentifier()) {
            return {}; // anonymous struct/union — skip
        }
        if (kind == CustPubKind::PubRepr) {
            // V40D-4: full body. Clang's RecordDecl::print() with
            // TerseOutput = false walks the AST and emits a full,
            // self-contained body (handles bitfields, anonymous
            // nested struct/union in one shot). This IS the
            // "custom pretty-printer over the AST" V40D-4
            // specifies — clang's printer is AST-driven, NOT
            // source-text copy, which is the property V40D-4
            // actually cares about. Slice B pre-validated via
            // `clang -Xclang -ast-print` that the output matches
            // V40D-4 §"Coverage" for most bullets.
            //
            // Two coverage gaps require manual emission because
            // clang's printer drops the attributes:
            //   * `__attribute__((packed))` — RecordDecl::hasAttr
            //   * `__attribute__((aligned(N)))` — AlignedAttr
            // Without these, the consumer TU sees a different
            // layout than the producer (ABI hazard). Emitted
            // after the closing `}` per V40D-4 ("after the
            // closing brace if … alignment exceeds the natural
            // alignment").
            policy.TerseOutput = false;
            rd->print(os, policy);
            // Strip trailing newline that print() emits so our
            // attribute suffix sits before the `;` on the same
            // closing-brace line. The trailing-newline behaviour
            // is consistent across clang versions covered by
            // V0.4.0's "Minimum clang: 19" header bullet.
            if (!out.empty() && out.back() == '\n') {
                out.pop_back();
            }
            if (rd->hasAttr<PackedAttr>()) {
                os << " __attribute__((packed))";
            }
            // AlignedAttr: a struct may carry multiple (per
            // `__attribute__((aligned(N)))` + `_Alignas`). Take
            // the maximum alignment specified.
            unsigned maxAlignBits = 0;
            for (const auto *aa : rd->specific_attrs<AlignedAttr>()) {
                if (aa->isAlignmentExpr()) {
                    unsigned a = aa->getAlignment(ctx);
                    if (a > maxAlignBits) maxAlignBits = a;
                }
            }
            if (maxAlignBits > 0) {
                os << " __attribute__((aligned(" << (maxAlignBits / 8)
                   << ")))";
            }
            os << ";\n";
        } else {
            // Opaque tag (plain pub).
            os << (rd->isUnion() ? "union " : "struct ");
            os << rd->getName() << ";\n";
        }
    } else if (const auto *ed = dyn_cast<EnumDecl>(D)) {
        if (!ed->getIdentifier()) {
            return {}; // anonymous enum — skip
        }
        if (kind == CustPubKind::PubRepr) {
            renderEnumBody(ed, os, policy);
        } else {
            // Plain pub: forward decl. C23 with fixed underlying
            // type accepts this directly; older standards / no
            // fixed type need a clang extension. Users who hit
            // C-compat issues here should use pub_repr.
            os << "enum " << ed->getName() << ";\n";
        }
    } else if (const auto *vd = dyn_cast<VarDecl>(D)) {
        if (kind == CustPubKind::PubRepr) {
            diagPubReprOnNonRecord(diags, vd->getLocation(),
                                   vd->getName(), "a variable");
            return {};
        }
        policy.TerseOutput = true;
        // Variable declarations become `extern <type> <name>;`.
        os << "extern ";
        vd->print(os, policy);
        os << ";\n";
    } else {
        return {};
    }
    return out;
}

// Collect the fragment-header contents for one TU.
std::string buildFragmentContents(ASTContext &ctx,
                                  DiagnosticsEngine &diags) {
    std::string contents;
    contents += "/* @generated by cust plugin — DO NOT EDIT */\n";
    contents += "/* Forward declarations of [[cust::pub]] items. */\n\n";

    for (Decl *D : ctx.getTranslationUnitDecl()->decls()) {
        if (D->isImplicit()) {
            continue;
        }
        CustPubKind kind = getCustPubKind(D);
        // Slice B emits Pub and PubRepr decls into the
        // fragment header. PubCrate is recognised by the
        // ParsedAttrInfo but its routing into the per-module
        // fragment header (with the /*c*/ prefix for the
        // concat-step filter) is slice D driver work; for
        // now PubCrate decls are silently dropped, matching
        // plugin v0's effective behaviour (today's
        // `hasCustPub` only matches the bare `cust::pub`
        // payload, so cust_pub_crate macros aren't emitted
        // anywhere today either).
        if (kind == CustPubKind::None || kind == CustPubKind::PubCrate) {
            continue;
        }
        std::string s = renderDecl(D, kind, ctx, diags);
        if (!s.empty()) {
            contents += s;
        }
    }
    return contents;
}

// V40D-6 + RQ-V40-2: emit one TSV line per discovered cust::test /
// cust::test_ignore decl. Format:
//
//   <qname>\t<fn_kind>\t<ignored>\t<file>\t<line>
//
// where qname = `<module>::<name>` (module passed in by the driver
// via the `module=...` plugin arg), fn_kind is the literal string
// `int` or `void`, ignored is `0` or `1`, file is an absolute path,
// line is a 1-based decimal. RQ-V40-2 rejects file paths containing
// literal tab or newline.
//
// Functions tagged but whose signature is not `int (void)` or
// `void (void)` are rejected here with a clear diagnostic. This
// matches the v0.3.2 pre-pass behaviour and keeps the runner
// template's switch on fn_kind closed (V40D-14 fn_ptr cast site).
std::string buildSidecarContents(ASTContext &ctx,
                                 llvm::StringRef module,
                                 DiagnosticsEngine &diags) {
    std::string contents;
    SourceManager &sm = ctx.getSourceManager();

    for (Decl *D : ctx.getTranslationUnitDecl()->decls()) {
        if (D->isImplicit()) {
            continue;
        }
        const auto *fd = dyn_cast<FunctionDecl>(D);
        if (!fd) {
            continue;
        }
        CustTestKind kind = getCustTestKind(fd);
        if (kind == CustTestKind::None) {
            continue;
        }

        // V40D-14 signature check. Test functions must take `void`
        // and return `int` or `void`; anything else is rejected.
        QualType ret = fd->getReturnType();
        llvm::StringRef fnKind;
        if (ret->isVoidType()) {
            fnKind = "void";
        } else if (ret->isSpecificBuiltinType(BuiltinType::Int)) {
            fnKind = "int";
        } else {
            unsigned id = diags.getCustomDiagID(
                DiagnosticsEngine::Error,
                "`[[cust::test]]` function `%0` must return `int` or "
                "`void` (got `%1`)");
            diags.Report(fd->getLocation(), id)
                << fd->getName().str() << ret.getAsString();
            continue;
        }
        if (fd->getNumParams() != 0) {
            unsigned id = diags.getCustomDiagID(
                DiagnosticsEngine::Error,
                "`[[cust::test]]` function `%0` must take no parameters "
                "(declared with `(void)`)");
            diags.Report(fd->getLocation(), id) << fd->getName().str();
            continue;
        }

        // RQ-V40-2 source-location -> file + line.
        PresumedLoc ploc = sm.getPresumedLoc(fd->getLocation());
        if (ploc.isInvalid()) {
            continue; // shouldn't happen for non-implicit decls
        }
        llvm::StringRef file = ploc.getFilename();
        if (file.contains('\t') || file.contains('\n')) {
            unsigned id = diags.getCustomDiagID(
                DiagnosticsEngine::Error,
                "sidecar file path contains illegal character "
                "(tab or newline): %0");
            diags.Report(fd->getLocation(), id) << file.str();
            continue;
        }

        contents += module.str();
        contents += "::";
        contents += fd->getName().str();
        contents += '\t';
        contents += fnKind.str();
        contents += '\t';
        contents += (kind == CustTestKind::TestIgnore) ? "1" : "0";
        contents += '\t';
        contents += file.str();
        contents += '\t';
        contents += std::to_string(ploc.getLine());
        contents += '\n';
    }
    return contents;
}

// Atomic write with skip-on-identical. Returns true on success.
bool writeFragmentIfChanged(const std::string &path,
                            const std::string &contents,
                            DiagnosticsEngine &diags) {
    // Skip if existing file matches byte-for-byte.
    {
        auto existing = llvm::MemoryBuffer::getFile(path);
        if (existing &&
            llvm::StringRef((*existing)->getBuffer()) == contents) {
            return true;
        }
    }

    // Ensure parent directory exists.
    std::error_code ec =
        llvm::sys::fs::create_directories(llvm::sys::path::parent_path(path));
    if (ec) {
        auto id = diags.getCustomDiagID(
            DiagnosticsEngine::Error,
            "cust plugin: failed to create directory for %0: %1");
        diags.Report(id) << path << ec.message();
        return false;
    }

    std::string tmp = path + ".tmp";
    {
        llvm::raw_fd_ostream os(tmp, ec);
        if (ec) {
            auto id = diags.getCustomDiagID(
                DiagnosticsEngine::Error,
                "cust plugin: failed to open %0 for write: %1");
            diags.Report(id) << tmp << ec.message();
            return false;
        }
        os << contents;
        // ~raw_fd_ostream flushes + closes.
    }

    ec = llvm::sys::fs::rename(tmp, path);
    if (ec) {
        auto id = diags.getCustomDiagID(
            DiagnosticsEngine::Error,
            "cust plugin: failed to rename %0 to %1: %2");
        diags.Report(id) << tmp << path << ec.message();
        return false;
    }
    return true;
}

class CustASTConsumer : public ASTConsumer {
public:
    CustASTConsumer(CompilerInstance &ci, std::string fragmentOut,
                    std::string testSidecarOut, std::string module)
        : CI(ci),
          FragmentOut(std::move(fragmentOut)),
          TestSidecarOut(std::move(testSidecarOut)),
          Module(std::move(module)) {}

    void HandleTranslationUnit(ASTContext &ctx) override {
        // V40D-5 phase isolation. Fragment headers AND test
        // discovery sidecars are written **only** in phase 1
        // (`ParseSyntaxOnly`). Phase 2 (`EmitLLVMOnly` /
        // `EmitObj`) with either path set is a driver bug —
        // hard error so the regression is impossible to miss.
        DiagnosticsEngine &diags = CI.getDiagnostics();
        bool phaseOne = CI.getFrontendOpts().ProgramAction ==
                        frontend::ParseSyntaxOnly;
        bool wantFragment = !FragmentOut.empty();
        bool wantSidecar = !TestSidecarOut.empty();

        if (!phaseOne && (wantFragment || wantSidecar)) {
            unsigned id = diags.getCustomDiagID(
                DiagnosticsEngine::Error,
                "cust plugin: phase-2 invocation must not write "
                "fragment headers or sidecar files (driver bug — "
                "drop -fplugin-arg-cust-{fragment-out,test-sidecar-out} "
                "for codegen invocations)");
            diags.Report(id);
            return;
        }

        // Phase 1 with no paths set is a silent no-op — IDE-side
        // clangd invocations want to share flags without the
        // side effects.
        if (!wantFragment && !wantSidecar) {
            return;
        }

        if (wantFragment) {
            std::string contents = buildFragmentContents(ctx, diags);
            writeFragmentIfChanged(FragmentOut, contents, diags);
        }
        if (wantSidecar) {
            if (Module.empty()) {
                unsigned id = diags.getCustomDiagID(
                    DiagnosticsEngine::Error,
                    "cust plugin: -fplugin-arg-cust-test-sidecar-out set "
                    "without -fplugin-arg-cust-module (driver bug)");
                diags.Report(id);
                return;
            }
            std::string contents = buildSidecarContents(ctx, Module, diags);
            writeFragmentIfChanged(TestSidecarOut, contents, diags);
        }
    }

private:
    CompilerInstance &CI;
    std::string FragmentOut;
    std::string TestSidecarOut;
    std::string Module;
};

// ---------------------------------------------------------------------------
// v0.4.0 slice A+B — `ParsedAttrInfo` recognisers for the five cust
// decl-annotation attributes.
//
// V40D-7 five-name model: one recogniser per attribute name (`cust::pub`,
// `cust::pub_crate`, `cust::pub_repr`, `cust::test`, `cust::test_ignore`),
// each accepting zero arguments. Earlier slice A draft used a
// parameterised `cust::pub` with `getArg(0)` dispatch on `crate` / `repr`;
// abandoned during slice A when AST-dump showed clang's expression-parser
// silently drops the identifier args (no scope binding for `crate` /
// `repr` → 0 ParsedAttr args at handleDeclAttribute time). The
// `ParsedAttrInfo` API has no hook to override the parser, so the
// five-separate-names model is the practical answer.
//
// Each recogniser attaches an `AnnotateAttr` with the corresponding
// `cust::*` payload string. The AST consumer (`getCustPubKind`) has a
// single recognition path keyed on the string. The legacy
// `annotate("cust::*")` macro path stays in place through slices A–D so
// cwork's prelude macros (which expand to the same payload strings) keep
// working; slice E retires the macros and removes the legacy path.
//
// Decl-kind-aware visibility lift (V40D-7): plain `[[cust::pub]]` on a
// FunctionDecl or VarDecl also gets `VisibilityAttr(Default)` to lift the
// symbol over the crate-wide `-fvisibility=hidden`. Type decls
// (typedef/record/enum) skip the lift (visibility on a typedef warns
// under -Wignored-attributes anyway). pub_crate / pub_repr never lift
// (V40D-3, V40D-4).

// Shared decl-kind check for all five recognisers.
bool custAttrAppertainsToDecl(Sema &S, const ParsedAttr &Attr,
                              const Decl *D, llvm::StringRef attrName) {
    if (!isa<FunctionDecl>(D) && !isa<VarDecl>(D) &&
        !isa<TypedefDecl>(D) && !isa<RecordDecl>(D) &&
        !isa<EnumDecl>(D)) {
        unsigned id = S.getDiagnostics().getCustomDiagID(
            DiagnosticsEngine::Error,
            "`[[%0]]` only applies to functions, variables, typedefs, "
            "structs, unions, and enums");
        S.Diag(Attr.getLoc(), id) << attrName.str();
        return false;
    }
    return true;
}

// Shared attach helper. liftVis = true for plain `[[cust::pub]]`; the
// recogniser caller filters by decl kind before passing true.
void custAttrAttach(Sema &S, Decl *D, const ParsedAttr &Attr,
                    llvm::StringRef payload, bool liftVis) {
    ASTContext &ctx = S.getASTContext();
    D->addAttr(AnnotateAttr::Create(ctx, payload.str(),
                                    /*Args=*/nullptr, /*NumArgs=*/0,
                                    Attr.getRange()));
    // V40D-7 slice E: marker required for AST consumer to
    // recognise the decl. Distinguishes plugin-attached payloads
    // from user-written `annotate("cust::pub")` strings.
    D->addAttr(AnnotateAttr::Create(ctx, kCustMarker.str(),
                                    /*Args=*/nullptr, /*NumArgs=*/0,
                                    Attr.getRange()));
    if (liftVis && (isa<FunctionDecl>(D) || isa<VarDecl>(D))) {
        D->addAttr(VisibilityAttr::CreateImplicit(
            ctx, VisibilityAttr::Default, Attr.getRange()));
    }
}

// `[[cust::pub]]` — plain. Lifts visibility on function/var.
struct CustPubAttrInfo : public ParsedAttrInfo {
    CustPubAttrInfo() {
        NumArgs = 0;
        OptArgs = 0;
        static constexpr Spelling kSpellings[] = {
            {ParsedAttr::AS_CXX11, "cust::pub"},
            {ParsedAttr::AS_C23, "cust::pub"},
        };
        Spellings = kSpellings;
    }

    bool diagAppertainsToDecl(Sema &S, const ParsedAttr &Attr,
                              const Decl *D) const override {
        return custAttrAppertainsToDecl(S, Attr, D, "cust::pub");
    }

    AttrHandling handleDeclAttribute(Sema &S, Decl *D,
                                     const ParsedAttr &Attr) const override {
        custAttrAttach(S, D, Attr, "cust::pub", /*liftVis=*/true);
        return AttributeApplied;
    }
};

// `[[cust::pub_crate]]` — no visibility lift (V40D-3: symbol will be
// localised at the crate-link step).
struct CustPubCrateAttrInfo : public ParsedAttrInfo {
    CustPubCrateAttrInfo() {
        NumArgs = 0;
        OptArgs = 0;
        static constexpr Spelling kSpellings[] = {
            {ParsedAttr::AS_CXX11, "cust::pub_crate"},
            {ParsedAttr::AS_C23, "cust::pub_crate"},
        };
        Spellings = kSpellings;
    }

    bool diagAppertainsToDecl(Sema &S, const ParsedAttr &Attr,
                              const Decl *D) const override {
        return custAttrAppertainsToDecl(S, Attr, D, "cust::pub_crate");
    }

    AttrHandling handleDeclAttribute(Sema &S, Decl *D,
                                     const ParsedAttr &Attr) const override {
        custAttrAttach(S, D, Attr, "cust::pub_crate", /*liftVis=*/false);
        return AttributeApplied;
    }
};

// `[[cust::pub_repr]]` — body export for record/enum (V40D-4). Validation
// of decl kind (must be record/enum, not function/var/typedef) happens in
// renderDecl with a richer diagnostic; here we accept any of the standard
// decl kinds and let renderDecl reject inappropriate ones with the
// V40D-4 hint wording.
struct CustPubReprAttrInfo : public ParsedAttrInfo {
    CustPubReprAttrInfo() {
        NumArgs = 0;
        OptArgs = 0;
        static constexpr Spelling kSpellings[] = {
            {ParsedAttr::AS_CXX11, "cust::pub_repr"},
            {ParsedAttr::AS_C23, "cust::pub_repr"},
        };
        Spellings = kSpellings;
    }

    bool diagAppertainsToDecl(Sema &S, const ParsedAttr &Attr,
                              const Decl *D) const override {
        return custAttrAppertainsToDecl(S, Attr, D, "cust::pub_repr");
    }

    AttrHandling handleDeclAttribute(Sema &S, Decl *D,
                                     const ParsedAttr &Attr) const override {
        custAttrAttach(S, D, Attr, "cust::pub_repr", /*liftVis=*/false);
        return AttributeApplied;
    }
};

// V40D-14: in non-test builds (`CUST_TEST_BUILD` undefined), test
// functions get `InternalLinkageAttr + UnusedAttr` so they don't
// leak into the regular artifact (no `nm` matches, no missing-
// reference warnings). In test builds (`CUST_TEST_BUILD` defined)
// they keep external linkage so the generated runner TU can
// reference them via `extern`.
//
// Macro detection from inside `handleDeclAttribute` (which runs
// during parsing, before `CustASTConsumer` exists):
//
//   S.getPreprocessor().getMacroInfo(
//       S.getPreprocessor().getIdentifierInfo("CUST_TEST_BUILD"))
//
// Returns non-null iff the macro is defined. Cached per-Sema via
// a static `llvm::DenseMap<Sema *, bool>` so a TU with many
// `[[cust::test]]` decls doesn't re-walk the macro table for
// each one. Sema lifetime is one TU; entries are short-lived.
bool isCustTestBuildDefined(Sema &S) {
    static llvm::DenseMap<Sema *, bool> cache;
    auto it = cache.find(&S);
    if (it != cache.end()) {
        return it->second;
    }
    Preprocessor &pp = S.getPreprocessor();
    IdentifierInfo *ii = pp.getIdentifierInfo("CUST_TEST_BUILD");
    bool defined = ii != nullptr && pp.getMacroInfo(ii) != nullptr;
    cache[&S] = defined;
    return defined;
}

void attachTestAttrs(Sema &S, Decl *D, const ParsedAttr &Attr,
                     llvm::StringRef payload) {
    ASTContext &ctx = S.getASTContext();
    D->addAttr(AnnotateAttr::Create(ctx, payload.str(),
                                    /*Args=*/nullptr, /*NumArgs=*/0,
                                    Attr.getRange()));
    // V40D-7 slice E marker; see custAttrAttach for rationale.
    D->addAttr(AnnotateAttr::Create(ctx, kCustMarker.str(),
                                    /*Args=*/nullptr, /*NumArgs=*/0,
                                    Attr.getRange()));
    if (!isCustTestBuildDefined(S)) {
        D->addAttr(InternalLinkageAttr::CreateImplicit(ctx, Attr.getRange()));
        D->addAttr(UnusedAttr::CreateImplicit(ctx, Attr.getRange()));
    }
}

// `[[cust::test]]` — function decls only. Validation of return
// type / param count lives in `buildSidecarContents` (richer
// diagnostic with the actual type); diagAppertainsToDecl just
// rejects non-functions.
struct CustTestAttrInfo : public ParsedAttrInfo {
    CustTestAttrInfo() {
        NumArgs = 0;
        OptArgs = 0;
        static constexpr Spelling kSpellings[] = {
            {ParsedAttr::AS_CXX11, "cust::test"},
            {ParsedAttr::AS_C23, "cust::test"},
        };
        Spellings = kSpellings;
    }

    bool diagAppertainsToDecl(Sema &S, const ParsedAttr &Attr,
                              const Decl *D) const override {
        if (!isa<FunctionDecl>(D)) {
            unsigned id = S.getDiagnostics().getCustomDiagID(
                DiagnosticsEngine::Error,
                "`[[cust::test]]` only applies to function declarations");
            S.Diag(Attr.getLoc(), id);
            return false;
        }
        return true;
    }

    AttrHandling handleDeclAttribute(Sema &S, Decl *D,
                                     const ParsedAttr &Attr) const override {
        attachTestAttrs(S, D, Attr, "cust::test");
        return AttributeApplied;
    }
};

// `[[cust::test_ignore]]` — same as test, just routes the sidecar
// `ignored` column to `1`.
struct CustTestIgnoreAttrInfo : public ParsedAttrInfo {
    CustTestIgnoreAttrInfo() {
        NumArgs = 0;
        OptArgs = 0;
        static constexpr Spelling kSpellings[] = {
            {ParsedAttr::AS_CXX11, "cust::test_ignore"},
            {ParsedAttr::AS_C23, "cust::test_ignore"},
        };
        Spellings = kSpellings;
    }

    bool diagAppertainsToDecl(Sema &S, const ParsedAttr &Attr,
                              const Decl *D) const override {
        if (!isa<FunctionDecl>(D)) {
            unsigned id = S.getDiagnostics().getCustomDiagID(
                DiagnosticsEngine::Error,
                "`[[cust::test_ignore]]` only applies to function "
                "declarations");
            S.Diag(Attr.getLoc(), id);
            return false;
        }
        return true;
    }

    AttrHandling handleDeclAttribute(Sema &S, Decl *D,
                                     const ParsedAttr &Attr) const override {
        attachTestAttrs(S, D, Attr, "cust::test_ignore");
        return AttributeApplied;
    }
};

class CustPluginAction : public PluginASTAction {
protected:
    std::unique_ptr<ASTConsumer> CreateASTConsumer(CompilerInstance &CI,
                                                   llvm::StringRef) override {
        return std::make_unique<CustASTConsumer>(CI, FragmentOut,
                                                 TestSidecarOut, Module);
    }

    bool ParseArgs(const CompilerInstance &CI,
                   const std::vector<std::string> &args) override {
        static constexpr llvm::StringRef kFragOut = "fragment-out=";
        static constexpr llvm::StringRef kSidecarOut = "test-sidecar-out=";
        static constexpr llvm::StringRef kModule = "module=";
        for (const auto &a : args) {
            llvm::StringRef sa = a;
            if (sa.starts_with(kFragOut)) {
                FragmentOut = sa.drop_front(kFragOut.size()).str();
            } else if (sa.starts_with(kSidecarOut)) {
                TestSidecarOut = sa.drop_front(kSidecarOut.size()).str();
            } else if (sa.starts_with(kModule)) {
                Module = sa.drop_front(kModule.size()).str();
            } else {
                auto &diags = CI.getDiagnostics();
                auto id = diags.getCustomDiagID(
                    DiagnosticsEngine::Warning,
                    "cust plugin: unknown arg %0 (ignored)");
                diags.Report(id) << a;
            }
        }
        return true;
    }

    // Run automatically every time the host frontend action runs,
    // matching the `-fplugin=...` invoker convention.
    PluginASTAction::ActionType getActionType() override {
        return AddBeforeMainAction;
    }

private:
    std::string FragmentOut;
    std::string TestSidecarOut;
    std::string Module;
};

} // namespace

static FrontendPluginRegistry::Add<CustPluginAction>
    X("cust", "cust clang plugin (surface extraction)");

// V40D-7 five-name model.
static ParsedAttrInfoRegistry::Add<CustPubAttrInfo>
    Y1("cust_pub", "cust [[cust::pub]] attribute recogniser (V40D-7)");
static ParsedAttrInfoRegistry::Add<CustPubCrateAttrInfo>
    Y2("cust_pub_crate", "cust [[cust::pub_crate]] attribute recogniser (V40D-7)");
static ParsedAttrInfoRegistry::Add<CustPubReprAttrInfo>
    Y3("cust_pub_repr", "cust [[cust::pub_repr]] attribute recogniser (V40D-7)");
static ParsedAttrInfoRegistry::Add<CustTestAttrInfo>
    Y4("cust_test", "cust [[cust::test]] attribute recogniser (V40D-7)");
static ParsedAttrInfoRegistry::Add<CustTestIgnoreAttrInfo>
    Y5("cust_test_ignore", "cust [[cust::test_ignore]] attribute recogniser (V40D-7)");

