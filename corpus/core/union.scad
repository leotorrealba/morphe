// Construct: union() of two overlapping solids.
union() {
    cube(10, center = true);
    translate([5, 5, 0]) sphere(r = 6, $fn = 32);
}
