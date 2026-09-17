//! Projections of a toolpath document into each harness's on-disk
//! session store. `p export <harness> --project` and `path resume`
//! write through these; the export command owns file and stdout
//! output.

pub(crate) mod claude;
#[cfg(test)]
pub(crate) mod test_support;
