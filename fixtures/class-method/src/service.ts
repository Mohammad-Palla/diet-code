class Local {
  live() {
    return this.help();
  }

  help() {
    return 1;
  }

  dead() {
    return 2;
  }
}

export function run() {
  return new Local().live();
}
