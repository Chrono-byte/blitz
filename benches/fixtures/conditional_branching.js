var sum = 0;
for (var i = 0; i < 50000; i = i + 1) {
  if (i % 2 === 0) {
    sum = sum + i;
  } else {
    sum = sum - i;
  }
}
