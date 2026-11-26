#!/usr/bin/env python3
"""Which screenshots differ between two runs of check.mjs.

    python3 scripts/phone/compare.py /tmp/before /tmp/after

For `--desktop` runs before and after a change: the phone's rules are meant
to leave the desktop exactly as it was, so any screen listed here is one to
look at. A clock or a "2 minutes ago" in the mock moves between runs, so a
difference of a few hundred pixels in one place can be that; the crop
written beside each pair (`*.diff.png`, the changed area only) says which.
"""
import sys
from pathlib import Path

from PIL import Image, ImageChops


def main(before: Path, after: Path) -> int:
    changed = 0
    for old in sorted(before.rglob('*.png')):
        if old.name.endswith('.diff.png'):
            continue
        new = after / old.relative_to(before)
        if not new.exists():
            print(f'missing   {old.relative_to(before)}')
            changed += 1
            continue
        a, b = Image.open(old).convert('RGB'), Image.open(new).convert('RGB')
        if a.size != b.size:
            print(f'resized   {old.relative_to(before)}  {a.size} -> {b.size}')
            changed += 1
            continue
        box = ImageChops.difference(a, b).getbbox()
        if box is None:
            continue
        diff = ImageChops.difference(a, b).convert('L').point(lambda v: 255 if v > 24 else 0)
        pixels = sum(1 for v in diff.getdata() if v)
        if pixels == 0:
            continue
        changed += 1
        crop = Image.new('RGB', (2 * (box[2] - box[0]) + 10, box[3] - box[1]), 'white')
        crop.paste(a.crop(box), (0, 0))
        crop.paste(b.crop(box), (box[2] - box[0] + 10, 0))
        out = new.with_suffix('.diff.png')
        crop.save(out)
        print(f'changed   {old.relative_to(before)}  {pixels} px in {box}  -> {out}')
    print(f'{changed} screen(s) differ' if changed else 'no differences')
    return 1 if changed else 0


if __name__ == '__main__':
    sys.exit(main(Path(sys.argv[1]), Path(sys.argv[2])))
