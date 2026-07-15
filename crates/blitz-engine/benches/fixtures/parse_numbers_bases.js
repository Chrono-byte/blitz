var sum = 0;
for (var i = 0; i < 10000; i = i + 1) {
  sum = sum + parseInt("FF", 16);
  sum = sum + parseInt("377", 8);
  sum = sum + parseInt("1010", 2);
  sum = sum + parseFloat("3.14159");
}
