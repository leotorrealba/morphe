// Real part: a control knob — a rounded body (hull of cylinders) with a
// D-shaft bore and finger grips.
// Constructs: hull, difference, union, for-loop, rotate, cylinder, cube, sphere.
body_r = 12;
body_h = 14;
shaft_r = 3;
grips = 12;

module rounded_body() {
    hull() {
        cylinder(h = body_h - 3, r = body_r, $fn = 64);
        translate([0, 0, body_h - 3]) cylinder(h = 0.1, r = body_r - 3, $fn = 64);
        translate([0, 0, body_h]) sphere(r = body_r - 5, $fn = 48);
    }
}

difference() {
    union() {
        rounded_body();
        // Finger grip ribs around the circumference.
        for (a = [0 : 360 / grips : 359]) {
            rotate([0, 0, a])
                translate([body_r - 1, 0, 0])
                    cylinder(h = body_h - 3, r = 1.2, $fn = 16);
        }
    }
    // D-shaped shaft bore.
    translate([0, 0, -1]) {
        cylinder(h = body_h, r = shaft_r, $fn = 32);
        translate([1.5, -shaft_r, 0]) cube([shaft_r, shaft_r * 2, body_h - 4]);
    }
}
