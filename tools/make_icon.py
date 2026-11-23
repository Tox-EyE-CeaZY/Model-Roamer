"""Draws the Model Roamer icon (assets/icon.png, icon-512.png, icon.ico, icon-64.rgba): the Roamer suite's chamfered
frame, with a lead diamond at the centre and three worker squares on tethers around it. Matches ui/favicon.svg."""
import math, os
from PIL import Image, ImageDraw

S = 1024  # drawn large, scaled down for smooth edges
LIME, INK, PALE, TEAL = (200, 255, 61, 255), (10, 17, 19, 255), (232, 240, 226, 255), (159, 208, 196, 255)
im = Image.new('RGBA', (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(im)
m, c = 56, 110
frame = [(m + c, m), (S - m, m), (S - m, S - m - c), (S - m - c, S - m), (m, S - m), (m, m + c)]
d.polygon(frame, fill=LIME)
w = 40
inner = [(m + w + c - 16, m + w), (S - m - w, m + w), (S - m - w, S - m - w - c + 16), (S - m - w - c + 16, S - m - w), (m + w, S - m - w), (m + w, m + w + c - 16)]
d.polygon(inner, fill=INK)

cx = cy = S // 2
R = 270      # orbit radius
sq = 120     # worker square size
# tethers from the lead to each worker, at 12, 4 and 8 o'clock
pts = [(cx + R * math.sin(math.radians(a)), cy - R * math.cos(math.radians(a))) for a in (0, 120, 240)]
for x, y in pts:
    d.line([cx, cy, x, y], fill=(92, 150, 140, 255), width=22)
# the lead: a diamond
L = 150
d.polygon([(cx, cy - L), (cx + L, cy), (cx, cy + L), (cx - L, cy)], fill=LIME, outline=INK, width=16)
# the workers
for i, (x, y) in enumerate(pts):
    d.rectangle([x - sq / 2, y - sq / 2, x + sq / 2, y + sq / 2], fill=LIME if i == 0 else TEAL, outline=INK, width=14)

out = os.path.join(os.path.dirname(__file__), '..', 'assets')
big = im.resize((256, 256), Image.LANCZOS)
big.save(os.path.join(out, 'icon.png'))
im.resize((512, 512), Image.LANCZOS).save(os.path.join(out, 'icon-512.png'))
big.save(os.path.join(out, 'icon.ico'), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
with open(os.path.join(out, 'icon-64.rgba'), 'wb') as f:
    f.write(im.resize((64, 64), Image.LANCZOS).tobytes())
