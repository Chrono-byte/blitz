var sum = 0;
var prod = 1;
for (var i = 1; i < 1000; i = i + 1) {
  sum = sum + i;
  prod = prod * ((i % 10) + 1);
}
