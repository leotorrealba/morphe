// use <file>: only the library's modules and functions are imported. Its
// demo peg is not rendered and `peg_radius` is not defined here, though the
// library's own functions and modules still see it.
use <lib/pegs.scad>

assert(is_undef(peg_radius), "use must not import variables");
translate([peg_double(5), 0, 0]) peg(h = 3, r = peg_default_radius());
cube(2);
