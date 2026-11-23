#!/bin/sh
# Regenerates the decode test fixtures (committed; ~100 KB total). Needs the ffmpeg CLI
# (built against Homebrew FFmpeg 9.0.2; hevc_videotoolbox needs macOS). VideoToolbox decodes the
# first three at 64x64, so the tests can assert the hardware path; FFV1 has no hardware decoder.
set -e
cd "$(dirname "$0")"
TAGS="-color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709"
F="-hide_banner -loglevel error -y"

# H.264 1 s 30 fps + 1 s AAC 440 Hz tone (audio for Task 8).
ffmpeg $F -f lavfi -i testsrc2=s=64x64:r=30:d=1 -f lavfi -i sine=f=440:d=1 \
  -c:v libx264 -pix_fmt yuv420p $TAGS -c:a aac -b:a 64k h264_64.mp4

# ProRes 4444 1 s 30 fps: red, alpha 0 in the left half, 1 in the right half.
ffmpeg $F -f lavfi -i color=c=red:s=64x64:r=30:d=1 \
  -f lavfi -i "color=c=white:s=64x64:r=30:d=1,drawbox=x=0:y=0:w=32:h=64:color=black:t=fill,format=gray" \
  -filter_complex "[0][1]alphamerge,format=yuva444p10le" \
  -c:v prores_ks -profile:v 4444 -alpha_bits 16 -pix_fmt yuva444p10le $TAGS prores4444_64.mov

# HEVC Main10 1 s 30 fps: white left half, 75 % grey (0xBF) right half; proves the P010 path.
ffmpeg $F -f lavfi -i "color=c=white:s=64x64:r=30:d=1,drawbox=x=32:y=0:w=32:h=64:color=0xBFBFBF:t=fill" \
  -c:v hevc_videotoolbox -profile:v main10 -pix_fmt p010le -tag:v hvc1 $TAGS hevc10_64.mp4

# FFV1 yuva420p 1 s: same red/alpha split as the ProRes clip, for the software (sws) fallback.
ffmpeg $F -f lavfi -i color=c=red:s=64x64:r=30:d=1 \
  -f lavfi -i "color=c=white:s=64x64:r=30:d=1,drawbox=x=0:y=0:w=32:h=64:color=black:t=fill,format=gray" \
  -filter_complex "[0][1]alphamerge,format=yuva420p" -c:v ffv1 -pix_fmt yuva420p $TAGS ffv1_alpha_64.mkv

# ProRes 422 HQ 1 s 30 fps, quadrants red | blue / green | white: VideoToolbox's p210le (4:2:2) path.
ffmpeg $F -f lavfi -i "color=c=red:s=64x64:r=30:d=1,drawbox=x=32:y=0:w=32:h=32:color=blue:t=fill,drawbox=x=0:y=32:w=32:h=32:color=0x00FF00:t=fill,drawbox=x=32:y=32:w=32:h=32:color=white:t=fill" \
  -c:v prores_ks -profile:v 3 -pix_fmt yuv422p10le $TAGS prores422hq_64.mov

# H.264 64x40 (coded 64x48) 1 s: white, the bottom 8 rows red. Hardware decoders may allocate the
# padded size: only the frame's 40 rows must be drawn (D3D12VA textures, `uv_scale`).
ffmpeg $F -f lavfi -i "color=c=white:s=64x40:r=30:d=1,drawbox=x=0:y=32:w=64:h=8:color=red:t=fill" \
  -c:v libx264 -pix_fmt yuv420p $TAGS h264_64x40.mp4

# H.264 1 s, index first (faststart), cut at 70 %: frames, then a decode error at the cut (I1).
ffmpeg $F -f lavfi -i testsrc2=s=64x64:r=30:d=1 -c:v libx264 -pix_fmt yuv420p $TAGS -movflags +faststart trunc_full.mp4
head -c $(( $(wc -c < trunc_full.mp4) * 7 / 10 )) trunc_full.mp4 > h264_trunc_64.mp4
rm trunc_full.mp4

# h264_64.mp4 with every AAC packet corrupted (deterministic): audio errors must not stop the video.
ffmpeg $F -i h264_64.mp4 -c copy -bsf:a noise=amount=2 h264_badaudio_64.mp4

# FFV1 planar YUV, quadrants red | blue / green | white: the software path that interleaves U and V
# into the YUV shader's planes (no RGBA conversion).
QUADS="color=c=red:s=64x64:r=30:d=1,drawbox=x=32:y=0:w=32:h=32:color=blue:t=fill,drawbox=x=0:y=32:w=32:h=32:color=0x00FF00:t=fill,drawbox=x=32:y=32:w=32:h=32:color=white:t=fill"
for fmt in yuv420p yuv422p10le yuv444p yuv444p10le; do
  ffmpeg $F -f lavfi -i "$QUADS" -c:v ffv1 -pix_fmt $fmt $TAGS ffv1_${fmt}_64.mkv
done

ls -l h264_64.mp4 h264_64x40.mp4 prores4444_64.mov hevc10_64.mp4 ffv1_alpha_64.mkv prores422hq_64.mov h264_trunc_64.mp4 h264_badaudio_64.mp4 ffv1_yuv*_64.mkv
