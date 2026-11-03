// Construct: a recursive function (Fibonacci) sizing a cube.
function fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2);
cube(fib(8));   // fib(8) = 21
