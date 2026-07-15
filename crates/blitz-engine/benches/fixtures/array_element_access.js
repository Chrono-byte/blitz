var arr = [1, 2, 3, 4, 5];
var sum = 0;
for (var i = 0; i < 10000; i = i + 1) {
  sum = sum + arr[0] + arr[1] + arr[2] + arr[3] + arr[4];
}
