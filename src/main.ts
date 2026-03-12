import { ok, err, type Result } from "neverthrow"
import * as R from "remeda"
import * as v from "valibot"

type UserId = number & { readonly _brand: "UserId" }
type RoleKind = "admin" | "user" | "guest"

type User = {
  readonly id: UserId
  readonly name: string
  readonly age: number
  readonly role: RoleKind
}

const UserSchema = v.object({
  id: v.number(),
  name: v.string(),
  age: v.number(),
  role: v.picklist(["admin", "user", "guest"]),
})

const parseUser = (raw: unknown): Result<User, string> => {
  const result = v.safeParse(UserSchema, raw)
  return result.success ? ok(result.output as User) : err(result.issues[0].message)
}

const users: readonly User[] = [
  { id: 1 as UserId, name: "Alice", age: 32, role: "admin" },
  { id: 2 as UserId, name: "Bob", age: 24, role: "user" },
  { id: 3 as UserId, name: "Carol", age: 28, role: "admin" },
  { id: 4 as UserId, name: "Dave", age: 19, role: "guest" },
  { id: 5 as UserId, name: "Eve", age: 24, role: "user" },
]

const adminNames: readonly string[] = R.pipe(
  users,
  R.filter((u) => u.role === "admin"),
  R.filter((u) => u.role === "admin"),
  R.map((u) => u.name),
)
