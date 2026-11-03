// include <file>: the library's text is inserted in place — its variables,
// its functions and modules, and its top-level geometry (the demo peg).
include <lib/pegs.scad>

translate([10, 0, 0]) cube(peg_double(peg_radius));
translate([0, 10, 0]) peg(h = 6);
