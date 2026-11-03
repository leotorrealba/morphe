// Construct: module definition + instantiation using children().
module frame(t = 2) {
    difference() {
        cube([20, 20, t]);
        translate([t, t, -1]) children();
    }
}
frame() cube([16, 16, 10]);
