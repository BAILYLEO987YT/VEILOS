"""
VEILOS Media Video (VMV) Converter
Converts MP4, MOV, AVI, and other video formats into VMV files
that can be played natively inside the VEILOS Media Viewer.

VMV Format:
  [4 bytes] Magic: "VMV\0"
  [2 bytes] Width  (u16 LE)
  [2 bytes] Height (u16 LE)
  [2 bytes] FPS    (u16 LE)
  [4 bytes] Frame count (u32 LE)
  For each frame:
    [4 bytes] Frame data length (u32 LE)
    [N bytes] JPEG-encoded frame data

Requirements:
  pip install opencv-python

Usage:
  python convert_to_vmv.py input.mp4 output.vmv [--fps 15] [--width 320] [--quality 75]
"""

import argparse
import struct
import sys

try:
    import cv2
except ImportError:
    print("ERROR: opencv-python is required.")
    print("Install it with: pip install opencv-python")
    sys.exit(1)


def convert_to_vmv(
    input_path: str,
    output_path: str,
    target_fps: int = 15,
    target_width: int = 0,
    jpeg_quality: int = 75,
):
    """
    Convert a video file to VEILOS VMV format.
    """
    cap = cv2.VideoCapture(input_path)
    if not cap.isOpened():
        print(f"ERROR: Cannot open video file: {input_path}")
        return False

    src_fps = cap.get(cv2.CAP_PROP_FPS)
    src_w = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
    src_h = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
    total = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))

    if target_fps <= 0:
        target_fps = int(src_fps) if src_fps > 0 else 15

    # Calculate frame skip ratio
    skip_ratio = src_fps / target_fps if src_fps > 0 else 1.0

    # Calculate target dimensions
    if target_width > 0 and target_width != src_w:
        scale = target_width / src_w
        out_w = target_width
        out_h = int(src_h * scale)
    else:
        out_w = src_w
        out_h = src_h

    print(f"Source:  {src_w}x{src_h} @ {src_fps:.1f} FPS ({total} frames)")
    print(f"Output:  {out_w}x{out_h} @ {target_fps} FPS, JPEG quality={jpeg_quality}")

    # Collect encoded frames
    frames_data = []
    frame_idx = 0
    next_target = 0.0

    while True:
        ret, frame = cap.read()
        if not ret:
            break

        if frame_idx >= next_target:
            # Resize if needed
            if out_w != src_w or out_h != src_h:
                frame = cv2.resize(
                    frame,
                    (out_w, out_h),
                    interpolation=cv2.INTER_AREA,
                )

            # Encode to JPEG
            encode_param = [cv2.IMWRITE_JPEG_QUALITY, jpeg_quality]
            success, encoded = cv2.imencode(".jpg", frame, encode_param)
            if success:
                frames_data.append(encoded.tobytes())

            next_target += skip_ratio

            # Progress
            if len(frames_data) % 50 == 0:
                print(f"  Encoded {len(frames_data)} frames...")

        frame_idx += 1

    cap.release()

    if not frames_data:
        print("ERROR: No frames were encoded.")
        return False

    print(f"  Total output frames: {len(frames_data)}")

    # Write VMV file
    with open(output_path, "wb") as f:
        # Header
        f.write(b"VMV\x00")
        f.write(struct.pack("<H", out_w))
        f.write(struct.pack("<H", out_h))
        f.write(struct.pack("<H", target_fps))
        f.write(struct.pack("<I", len(frames_data)))

        # Frames
        for fd in frames_data:
            f.write(struct.pack("<I", len(fd)))
            f.write(fd)

    total_size = sum(len(fd) for fd in frames_data) + 14 + 4 * len(frames_data)
    print(f"  Written: {output_path} ({total_size / 1024:.1f} KB)")
    return True


def main():
    parser = argparse.ArgumentParser(
        description="Convert video files to VEILOS VMV format",
    )
    parser.add_argument(
        "input",
        help="Input video file (MP4, MOV, AVI, etc.)",
    )
    parser.add_argument(
        "output",
        help="Output VMV file path",
    )
    parser.add_argument(
        "--fps",
        type=int,
        default=15,
        help="Target FPS (default: 15). Lower = smaller file.",
    )
    parser.add_argument(
        "--width",
        type=int,
        default=320,
        help="Target width in pixels (default: 320). Height scales proportionally.",
    )
    parser.add_argument(
        "--quality",
        type=int,
        default=75,
        help="JPEG quality 1-100 (default: 75). Lower = smaller file.",
    )

    args = parser.parse_args()

    success = convert_to_vmv(
        args.input,
        args.output,
        target_fps=args.fps,
        target_width=args.width,
        jpeg_quality=args.quality,
    )

    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()
