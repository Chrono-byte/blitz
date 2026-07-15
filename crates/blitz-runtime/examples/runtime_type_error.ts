function badReturn(): number {
  return "not a number";
  return 1;
}
console.log(badReturn());
