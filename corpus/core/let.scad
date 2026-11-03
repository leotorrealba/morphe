// Construct: let() introducing local bindings inside an expression.
size = let (a = 4, b = a * 2) a + b;
cube(size);
