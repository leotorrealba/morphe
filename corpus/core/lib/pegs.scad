// A small library, pulled in by include.scad (`include`) and use.scad
// (`use`). Run on its own it renders its demo peg.

// File-scope variable: visible to this file's functions and modules even
// when the file is `use`d, but never to the file that uses it.
peg_radius = 2;

function peg_double(x) = x * 2;
function peg_default_radius() = peg_radius;

module peg(h = 4, r = peg_radius) cylinder(h = h, r = r, $fn = 24);

// Demo geometry: part of the model when this file is included or run
// directly, never when it is used.
peg(1);
