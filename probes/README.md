# Probes

Jobs whose *answers* are readable off a SuperNesting job report, built to work
out where its advantage actually comes from. Not fixtures - nothing in the
test suite touches these.

## Concavity

`probe_solid.dxf` and `probe_bitten.dxf` are the same 280x150 rectangle with
and without `nestTest03`'s exact quarter-disc bite (R100 at one corner):
42,000 mm² against 34,146 mm², 19% less material per part.

Nest both at **qty 250, 1500x1500, margin 0, spacing 5**. If the bitten one
does not beat the solid one on parts-per-sheet, that engine is packing
bounding boxes and ignoring concavity.

Ours: solid **48** parts/sheet, bitten **49**, both 6 sheets - we barely
exploit it either.

The arc is tessellated at 16 segments deliberately. A 65-segment version of
the identical outline packed *worse* (49 against 52 parts/sheet) and stopped
matching `nestTest03`'s own behaviour, so the two files here are built the
same way and are comparable to each other and to the fixture.

`make_probe.py` regenerates both.
