/* Module `a`: one half of the 2-cycle. Imports `b` and exports a
 * pub_repr struct pointing at `b`'s struct. */
#cust use crate::b;

struct [[cust::pub_repr]] ca {
    struct cb *peer;
    int v;
};

[[cust::pub]] int ca_value(struct ca *s) {
    return s->v;
}
