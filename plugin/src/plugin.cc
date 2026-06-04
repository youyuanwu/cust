// cust clang plugin — v0.2 scaffold.
//
// Minimal in-process clang plugin. Registers itself under the
// name "cust" so it can be loaded with
//
//     clang -fplugin=./libcust_plugin.so -fplugin-arg-cust-phase=surface
//
// Today the plugin just emits a remark on every TU so we can
// verify the loading path end-to-end. Fragment-header synthesis,
// AST inspection, and the surface/codegen phase split land in
// follow-up commits per docs/design/v0.2.md.

#include "clang/AST/ASTConsumer.h"
#include "clang/Frontend/CompilerInstance.h"
#include "clang/Frontend/FrontendPluginRegistry.h"
#include "llvm/Support/raw_ostream.h"

#include <memory>
#include <string>
#include <vector>

using namespace clang;

namespace {

class CustASTConsumer : public ASTConsumer {
public:
    explicit CustASTConsumer(CompilerInstance &ci) : CI(ci) {}

    void HandleTranslationUnit(ASTContext &) override {
        // No-op for now. The surface-extraction pass (emitting
        // <module>.cust.h fragment headers) lives here in a
        // follow-up commit.
        auto &diags = CI.getDiagnostics();
        auto id = diags.getCustomDiagID(
            DiagnosticsEngine::Remark,
            "cust plugin loaded (v0.2 scaffold; no-op)");
        diags.Report(id);
    }

private:
    CompilerInstance &CI;
};

class CustPluginAction : public PluginASTAction {
protected:
    std::unique_ptr<ASTConsumer> CreateASTConsumer(CompilerInstance &CI,
                                                   llvm::StringRef) override {
        return std::make_unique<CustASTConsumer>(CI);
    }

    // Plugin args (e.g. -fplugin-arg-cust-phase=surface). Today
    // we just accept any args without complaint; the
    // surface/codegen phase switch lands when the plugin actually
    // emits fragment headers.
    bool ParseArgs(const CompilerInstance &,
                   const std::vector<std::string> &args) override {
        for (const auto &a : args) {
            (void)a; // accept silently
        }
        return true;
    }

    // Run the plugin automatically every time the host frontend
    // action runs, instead of requiring `-Xclang -add-plugin
    // cust`. This is what `-fplugin=...` invokers expect.
    PluginASTAction::ActionType getActionType() override {
        return AddBeforeMainAction;
    }
};

} // namespace

static FrontendPluginRegistry::Add<CustPluginAction>
    X("cust", "cust clang plugin (v0.2 scaffold)");
