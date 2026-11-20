#!/bin/sh
# Regenerates the decode test fixtures (committed; ~52 KB total). Needs the ffmpeg CLI
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

ls -l h264_64.mp4 prores4444_64.mov hevc10_64.mp4 ffv1_alpha_64.mkv
