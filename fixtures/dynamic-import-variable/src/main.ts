async function load(n: string) {
  const mod = await import(`./plugins/${n}.js`);
  return mod;
}

load("a");
