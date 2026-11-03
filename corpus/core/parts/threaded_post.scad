// Real part: a threaded-look standoff post built as a rotate_extrude of a
// serrated profile, on a hex base.
// Constructs: rotate_extrude, polygon, difference, union, for-loop, cylinder, linear_extrude, circle.
post_h = 24;
core_r = 4;
teeth = 24;

module thread_body() {
    // A saw-tooth radial profile swept around Z approximates a thread ridge.
    rotate_extrude($fn = 96)
        polygon(points = [
            [0, 0],
            [core_r + 1, 0],
            [core_r, post_h * 0.5],
            [core_r + 1, post_h],
            [0, post_h]
        ]);
}

union() {
    // Hex base.
    linear_extrude(height = 4) circle(r = 8, $fn = 6);
    translate([0, 0, 4]) difference() {
        thread_body();
        // Axial pilot hole.
        translate([0, 0, -1]) cylinder(h = post_h + 2, r = 1.5, $fn = 24);
    }
}
