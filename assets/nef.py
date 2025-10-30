import os
import io
import rawpy
from PIL import Image, ImageOps
import math
import argparse
import time
import signal
from concurrent.futures import ThreadPoolExecutor
from threading import Event, Lock
from queue import Queue

# --------------------
# The original python file
# fempeg was inspired by and reuses parts of the code from nef.py
# This file was written by Markofwitch about 7 months before fempeg
# --------------------

VALID_FORMATS = {'png', 'jpeg', 'jpg', 'bmp', 'gif', 'webp'}
stop_event = Event()
print_queue = Queue()
progress_lock = Lock()


def resize_image(img, ratio):
    scale = math.sqrt(ratio)
    new_size = (int(img.width * scale), int(img.height * scale))
    return img.resize(new_size, Image.LANCZOS)


def apply_exif_orientation(img):
    try:
        exif = img.getexif()
        orientation = exif.get(0x0112)
        if orientation == 2:
            img = ImageOps.mirror(img)
        elif orientation == 3:
            img = img.rotate(180, expand=True)
        elif orientation == 4:
            img = ImageOps.flip(img)
        elif orientation == 5:
            img = ImageOps.mirror(img.rotate(-90, expand=True))
        elif orientation == 6:
            img = img.rotate(-90, expand=True)
        elif orientation == 7:
            img = ImageOps.mirror(img.rotate(90, expand=True))
        elif orientation == 8:
            img = img.rotate(90, expand=True)
    except Exception:
        pass
    return img


def convert_nef(in_path, out_path, out_format, resize_ratio, no_raw):
    with rawpy.imread(in_path) as raw:
        if no_raw:
            try:
                thumb = raw.extract_thumb()
                if thumb.format == rawpy.ThumbFormat.JPEG:
                    img = Image.open(io.BytesIO(thumb.data))
                    img = apply_exif_orientation(img)
                elif thumb.format == rawpy.ThumbFormat.BITMAP:
                    img = Image.fromarray(thumb.data)
                else:
                    raise RuntimeError('Unsupported thumbnail format')
            except Exception as e:
                raise RuntimeError(f'Failed to extract preview: {e}')
        else:
            rgb = raw.postprocess(use_camera_wb=True, no_auto_bright=True)
            img = Image.fromarray(rgb)

    img = resize_image(img, resize_ratio)
    img.save(out_path, format=out_format.upper())


def format_time(seconds):
    seconds = int(seconds)
    return f'{seconds}s' if seconds < 60 else f'{seconds // 60}m {seconds % 60}s'


def handle_interrupt(signum, frame):
    print_queue.put('Received interrupt. Stopping after current conversions...\n')
    stop_event.set()


def worker(index, total, fname, in_path, out_dirs, out_formats, ratio, start_time, counter, no_raw):
    if stop_event.is_set():
        return

    t0 = time.perf_counter()
    try:
        for out_format, out_dir in zip(out_formats, out_dirs):
            out_name = f'{os.path.splitext(fname)[0]}.{out_format}'
            out_path = os.path.join(out_dir, out_name)
            convert_nef(in_path, out_path, out_format, ratio, no_raw)

        elapsed = time.perf_counter() - t0
        with progress_lock:
            counter[0] += 1
            avg_time = (time.perf_counter() - start_time) / counter[0]
            remaining = avg_time * (total - counter[0])

        msg = f'[{index}/{total}] {fname} → {"+".join([f.upper() for f in out_formats])}... Done ({format_time(elapsed)})'
        msg += f'\n   ↳ Est. time left: {format_time(remaining)}'
        print_queue.put(msg)
    except Exception as e:
        print_queue.put(f'[{index}/{total}] {fname}... Error: {e}')


def main():
    signal.signal(signal.SIGINT, handle_interrupt)

    parser = argparse.ArgumentParser(description='Convert NEF images to common formats.')
    parser.add_argument('input_dir', help='Input directory with .nef files')
    parser.add_argument('output_dir', help='Directory to save converted images')
    parser.add_argument('--format', '-f', default='png', help='Output format(s): e.g. png, jpeg, or png+jpg')
    parser.add_argument('--ratio', '-r', type=float, default=0.15, help='Resize ratio (0 < ratio ≤ 1.0)')
    parser.add_argument('--threads', '-t', type=int, default=os.cpu_count(), help='Number of threads to use')
    parser.add_argument('--no-raw', action='store_true', help='Use embedded preview instead of full raw processing')

    args = parser.parse_args()

    out_formats = [f.lower().replace('jpg', 'jpeg') for f in args.format.split('+')]
    for fmt in out_formats:
        if fmt not in VALID_FORMATS:
            raise ValueError(f'Unsupported format: {fmt}. Valid: {", ".join(VALID_FORMATS)}')
    if not (0 < args.ratio <= 1.0):
        raise ValueError('Resize ratio must be between 0 and 1')

    out_dirs = []
    for fmt in out_formats:
        dir_path = os.path.join(args.output_dir, fmt)
        os.makedirs(dir_path, exist_ok=True)
        out_dirs.append(dir_path)

    nef_files = [f for f in os.listdir(args.input_dir) if f.lower().endswith('.nef')]
    total = len(nef_files)
    if total == 0:
        print('No .nef files found.')
        return

    print(f'Found {total} NEF files. Starting conversion with {args.threads} thread(s)...\n')

    start_time = time.perf_counter()
    counter = [0]

    if args.threads == 1:
        for i, fname in enumerate(nef_files, 1):
            if stop_event.is_set():
                break
            in_path = os.path.join(args.input_dir, fname)
            worker(i, total, fname, in_path, out_dirs, out_formats, args.ratio, start_time, counter, args.no_raw)

            while not print_queue.empty():
                print(print_queue.get())
    else:
        with ThreadPoolExecutor(max_workers=args.threads) as executor:
            for i, fname in enumerate(nef_files, 1):
                if stop_event.is_set():
                    break
                in_path = os.path.join(args.input_dir, fname)
                executor.submit(worker, i, total, fname, in_path, out_dirs, out_formats, args.ratio, start_time, counter, args.no_raw)

            while counter[0] < total and not stop_event.is_set():
                try:
                    msg = print_queue.get(timeout=0.1)
                    print(msg)
                except:
                    pass

    total_time = time.perf_counter() - start_time
    if stop_event.is_set():
        print('\nStopped early.')
    else:
        print('\nAll conversions completed.')

    print(f'Total execution time: {format_time(total_time)}')


if __name__ == '__main__':
    main()
