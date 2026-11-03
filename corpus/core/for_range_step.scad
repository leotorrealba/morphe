// Construct: for-loop over a range with an explicit step.
for (z = [0 : 5 : 20]) {
    translate([0, 0, z]) cylinder(h = 3, r = 5, $fn = 24);
}
