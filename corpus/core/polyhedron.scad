// Construct: polyhedron() — an explicit tetrahedron from points + faces.
polyhedron(
    points = [[0, 0, 0], [10, 0, 0], [5, 10, 0], [5, 5, 10]],
    faces  = [[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]]
);
