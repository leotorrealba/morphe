// Construct: a recursive module building a tapering stack.
module tower(n, w) {
    if (n > 0) {
        translate([0, 0, (5 - n) * 3]) cube([w, w, 3], center = false);
        tower(n - 1, w * 0.8);
    }
}
tower(5, 10);
