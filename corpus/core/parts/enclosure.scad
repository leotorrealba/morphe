// Real part: a snap-box enclosure — an outer shell hollowed by an inner
// cavity (difference of shells) with a cable cutout and internal boss posts.
// Constructs: difference, union, for-loop, translate, cube, cylinder.
outer = [50, 34, 20];
wall = 2;

module shell() {
    difference() {
        cube(outer);
        // Hollow the inside, leaving a floor and four walls (open top).
        translate([wall, wall, wall])
            cube([outer[0] - 2 * wall, outer[1] - 2 * wall, outer[2]]);
        // Cable pass-through on the front wall.
        translate([outer[0] / 2 - 5, -1, outer[2] - 8])
            cube([10, wall + 2, 6]);
    }
}

module bosses() {
    for (c = [[6, 6], [outer[0] - 6, 6], [6, outer[1] - 6], [outer[0] - 6, outer[1] - 6]]) {
        translate([c[0], c[1], wall]) difference() {
            cylinder(h = outer[2] - wall - 2, r = 3, $fn = 24);
            translate([0, 0, 2]) cylinder(h = outer[2], r = 1.2, $fn = 16);
        }
    }
}

union() {
    shell();
    bosses();
}
