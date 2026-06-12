pub mod aac {
    pub const EXTENSION: &str = "m4a";
    pub const SAMPLE_RATE: usize = 44100;
    pub const FRAME_SIZE: usize = 1024;
}

pub mod opus {
    pub const EXTENSION: &str = "opus";
    pub const SAMPLE_RATE: usize = 48000;
    pub const FRAME_SIZE: usize = (SAMPLE_RATE as f32 * FRAME_SIZE_MS / 1000.0).round() as usize;
    pub const SERIAL: u32 = 0x1FEE1BAD;
    pub const VENDOR_STRING: &[u8] = b"Aprcot by Vinyl";

    const FRAME_SIZE_MS: f32 = 20.0;
}

pub mod vorbis {
    pub const EXTENSION: &str = "ogg";
    pub const SAMPLE_RATE: usize = 44100;
    pub const FRAME_SIZE: usize = 1024;
    pub const SERIAL: i32 = 0x1FEE1BAD;
}
