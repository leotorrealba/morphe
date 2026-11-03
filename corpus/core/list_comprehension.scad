// Construct: list comprehension generating translate offsets.
offsets = [for (i = [0 : 5]) i * i];
for (o = offsets) {
    translate([o, 0, 0]) cube(3);
}
