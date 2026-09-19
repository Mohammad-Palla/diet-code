async function load() {
  const mod = await import("./plugin.js");
  return mod.run();
}

load();
