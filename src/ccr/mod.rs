pub mod codec;
pub mod detect;
pub mod zstd_codec;

#[allow(unused_imports)]
pub use codec::{codec_by_id, codec_for, Codec};
#[allow(unused_imports)]
pub use detect::ContentType;
