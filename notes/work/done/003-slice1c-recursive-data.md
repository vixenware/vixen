# 003: Slice 1c — recursive data structures

Depends on: 001 (structs), 002 (enums).

With structs + enums + match, we can build recursive data: linked lists, trees.

```vixen
enum List {
  struct Cons { head: U64, tail: List }
  struct Nil {}
}

fn sum(list: List) -> U64 (
  match list (
    Cons { head, tail } => head + sum(tail),
    Nil {} => 0,
  )
)

fn main() -> U64 (
  sum(Cons { head: 1, tail: Cons { head: 2, tail: Cons { head: 3, tail: Nil {} } } })
)
```

## Key tests

- Build a list, sum it
- Build a list with a bomb in the tail, only access head — demand-driven
- Build a tree, traverse only left branch
- Infinite-ish structure: only force what's needed

## Lazy recursive data

```vixen
fn main() -> U64 (
  let list = Cons { head: 42, tail: bomb_list() };
  match list (
    Cons { head, tail } => head,
    Nil {} => 0,
  )
)
```

Only `head` is demanded — `tail` (and `bomb_list()`) never forced.
This is where lazy evaluation on structured data really shines.

## Status: DONE

All 8 fixtures pass (slice1 08–15):

- `08_list_sum` — linked list sum via recursive match
- `09_list_head_only` — lazy: only access head, tail has bomb()
- `10_tree_left_only` — lazy: traverse only left branch, right has bomb()
- `11_list_length` — recursive list length
- `12_nested_option` — match Option, dead branch has bomb()
- `13_tree_sum_leaves` — sum all leaves in binary tree
- `14_list_map` — map double over list then sum
- `15_lazy_spine` — first element of list where second element's head is bomb()
