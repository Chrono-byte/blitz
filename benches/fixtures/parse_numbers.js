var sum = 0;
for (var i = 0; i < 50000; i = i + 1) {
  sum = sum + parseInt("12345", 10);
  sum = sum + parseFloat("67890.25");
}
