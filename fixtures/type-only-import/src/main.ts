import type { User } from "./user";

export function greet(u: User) {
  return u.name;
}

greet({ name: "ada" });
