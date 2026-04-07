const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

sleep(1000).then(() => {
  console.log("hello!")
});
