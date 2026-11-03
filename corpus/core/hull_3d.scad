// Construct: hull() over 3D solids (a rounded capsule).
hull() {
    translate([-10, 0, 0]) sphere(r = 4, $fn = 32);
    translate([10, 0, 0]) sphere(r = 4, $fn = 32);
}
