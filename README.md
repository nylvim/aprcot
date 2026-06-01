# aprcot

Aprcot is an audio transcoder, built for compressing music files in order to take less space on the disk. It can also resize and transcode cover images to further reduce file sizes.

The name "aprcot" stands for **A**udio **Pr**ocessor, **Co**mpressor and **T**ranscoder. (no, it does not actually do any processing to the audio, it's just for the acronym)

Currently it supports all major audio formats and NCM (NetEase's proprietary format) as input, and Opus, Vorbis or AAC as output. Support for more formats is being added.

## How to choose codec

**Size effeciency (high quality):** Opus >= xHE-AAC > AAC > Vorbis >> MP3

**Compatibility:** MP3 > AAC >= Vorbis > Opus > xHE-AAC

Newer software players should handle all codecs well.

## Known issues

- MP3 cover images are not present in the output. (upstream issue)
- Opus mode is ~3x slower than `opusenc`. (you can lower the encoding complexity)
- This program only preserves some of the metadata tags. Welcome to open an issue if you find any important tag missing after transcoding. (I'm just too lazy)
