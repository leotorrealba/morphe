// Construct: for-loop over a range placing a row of cubes.
for (i = [0 : 4]) {
    translate([i * 6, 0, 0]) cube(4);
}
