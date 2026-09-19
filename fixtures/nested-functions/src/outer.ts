export function outer() {
  innerUsed();
  return innerUsed();

  function innerUsed() {
    return 1;
  }

  function inner() {
    return 2;
  }
}
