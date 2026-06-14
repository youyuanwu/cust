/* Module `b`: the other half of the 2-cycle. Imports `a` and
 * exports a pub_repr struct pointing at `a`'s struct. */
#cust use crate::a;

struct [[cust::pub_repr]] cb {
    struct ca *peer;
    int v;
};

[[cust::pub]] int cb_value(struct cb *s) {
    return s->v;
}
