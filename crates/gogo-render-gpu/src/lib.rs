//! The vello_gpu renderer on wgpu (design 28). Sees only the display list (design 24 §2.1
//! rule 3). The only GUI crate that may use `unsafe`: creating wgpu's pipeline cache from
//! bytes (design 28 §4). Filled in by PR C5 (#530).
