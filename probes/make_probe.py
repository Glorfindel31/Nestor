"""Writes the two concavity-probe DXFs: the same 280x150 part with and
without its 100mm quarter-circle bite. Minimal R12 ENTITIES-only DXF."""
import io
import math
import os

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)))
os.makedirs(OUT, exist_ok=True)

W, H, R = 280.0, 150.0, 100.0


def lwpolyline(points, layer='0'):
    out = ['0', 'LWPOLYLINE', '8', layer, '100', 'AcDbEntity', '100', 'AcDbPolyline',
           '90', str(len(points)), '70', '1']
    for x, y in points:
        out += ['10', '%.6f' % x, '20', '%.6f' % y]
    return out


def dxf(entities):
    out = ['0', 'SECTION', '2', 'ENTITIES']
    out += entities
    out += ['0', 'ENDSEC', '0', 'EOF']
    return '\n'.join('  ' + v if i % 2 == 0 else v for i, v in enumerate(out)) + '\n'


# nestTest03 is this 280x150 rectangle with a quarter disc of radius 100
# taken out of the (W, 0) corner - the arc runs at radius R about that corner.
solid = [(0.0, 0.0), (0.0, H), (W, H), (W, 0.0)]

bitten = [(0.0, 0.0), (0.0, H), (W, H), (W, R)]
for k in range(0, 17):
    a = math.radians(90.0 + 90.0 * k / 16.0)
    bitten.append((W + R * math.cos(a), R * math.sin(a)))

io.open(os.path.join(OUT, 'probe_solid.dxf'), 'w').write(dxf(lwpolyline(solid)))
io.open(os.path.join(OUT, 'probe_bitten.dxf'), 'w').write(dxf(lwpolyline(bitten)))
print('wrote', OUT)
