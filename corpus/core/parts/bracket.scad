// Real part: an L-bracket plate with four mounting holes and a lightening slot.
// Constructs: difference, union, for-loop, translate, rotate, cylinder, cube.
thickness = 4;
plate_x = 40;
plate_y = 30;
hole_r = 2.5;
inset = 5;

module mount_holes() {
    for (x = [inset, plate_x - inset]) {
        for (y = [inset, plate_y - inset]) {
            translate([x, y, -1]) cylinder(h = thickness + 2, r = hole_r, $fn = 24);
        }
    }
}

difference() {
    union() {
        cube([plate_x, plate_y, thickness]);
        // Upstanding flange along the back edge.
        translate([0, plate_y - thickness, 0])
            cube([plate_x, thickness, 20]);
    }
    mount_holes();
    // Central lightening slot.
    translate([plate_x / 2 - 8, plate_y / 2 - 3, -1])
        cube([16, 6, thickness + 2]);
}
