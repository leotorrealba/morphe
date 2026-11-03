// Construct: hull() over 2D profiles — a 2D result (expected kind: profile).
hull() {
    translate([-10, 0]) circle(r = 4, $fn = 32);
    translate([10, 0]) circle(r = 4, $fn = 32);
}
