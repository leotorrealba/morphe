// Construct: nested for-loops tiling a grid of pillars.
for (x = [0 : 3]) {
    for (y = [0 : 3]) {
        translate([x * 6, y * 6, 0]) cube([4, 4, 8]);
    }
}
