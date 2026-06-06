// v0.4.0 slice B fixture — pub_repr body export coverage (V40D-4).
//
// Verified byte-for-byte against `goldens/test_pub_repr_body.cust.h`
// by `run_golden_test.sh`. Update the golden whenever the fixture
// or the plugin's pretty-printer changes (V40D-8 stamping
// invariant: byte-stable output across plugin rebuilds is what
// the §4 stamping skip relies on).

// Basic fields.
struct [[cust::pub_repr]] basic_s {
    int x;
    int y;
    char tag;
};

// Bitfields.
struct [[cust::pub_repr]] bits_s {
    unsigned hi : 4;
    unsigned lo : 4;
    int rest;
};

// Packed struct.
struct [[cust::pub_repr]] __attribute__((packed)) packed_s {
    char a;
    int  b;
    char c;
};

// Over-aligned struct.
struct [[cust::pub_repr]] __attribute__((aligned(16))) aligned_s {
    int x;
};

// Anonymous nested union — V40D-4 "anonymous nested struct/union:
// emitted in-line at the field site, recursively."
struct [[cust::pub_repr]] nested_anon_s {
    int kind;
    union {
        int  i;
        long l;
    };
};

// Union top-level — V40D-4 "Unions: same as structs, with `union`
// keyword."
union [[cust::pub_repr]] simple_u {
    int   i;
    float f;
};

// Enum with explicit discriminants (already explicit; should pass
// through).
enum [[cust::pub_repr]] kind_e {
    KIND_A = 1,
    KIND_B = 2,
    KIND_C = 4,
};

// Enum with implicit discriminants — V40D-4 "Discriminant values
// always emitted explicitly (not implicit) so a future enum-variant
// reorder doesn't change the public ABI silently." The plugin must
// inject `= 0`, `= 1`, `= 2` even though the user wrote bare names.
enum [[cust::pub_repr]] implicit_e {
    IMP_FIRST,
    IMP_SECOND,
    IMP_THIRD,
};

// Enum with C23 fixed underlying type.
enum [[cust::pub_repr]] byte_e : unsigned char {
    BYTE_LO = 0,
    BYTE_HI = 255,
};
