---
title: Language Reference
permalink: /reference/
---

# Language Reference

Jam is a statically typed systems language with mutable value semantics, generics
over types, tagged unions, and an LLVM backend. The compiler infers types where it
can; everything across a function boundary is fully explicit.
{: .lead}

The reference is split into six pages. Read them in order for a tour of the language,
or jump to the one you need.

- **[Basics]({{ '/reference/basics/' | relative_url }}).** Bindings and the built-in
  types: integers, booleans, strings, pointers, slices and arrays.
- **[Control Flow & Functions]({{ '/reference/control-flow-functions/' | relative_url }}).**
  Branches and loops, function declarations, function pointers, and casts.
- **[Data Types]({{ '/reference/data-types/' | relative_url }}).** Structs, enums,
  unions, pattern matching and generics.
- **[Mutable Value Semantics]({{ '/reference/value-semantics/' | relative_url }}).**
  Parameter modes, exclusivity and drop: how Jam stays memory safe without lifetimes.
- **[Compile Time]({{ '/reference/compile-time/' | relative_url }}).** Compile-time
  execution and the intrinsics available to it.
- **[Modules, Testing & FFI]({{ '/reference/modules/' | relative_url }}).** Imports,
  tests, and calling C through `extern`.

<script>
// The reference used to be one page. Send old section links to their new home.
(function () {
    var moved = {
        'variables': 'basics',
        'types': 'basics',
        'bool': 'basics',
        'strings': 'basics',
        'pointers': 'basics',
        'arrays': 'basics',
        'control-flow': 'control-flow-functions',
        'fn-declaration': 'control-flow-functions',
        'fn-pointers': 'control-flow-functions',
        'casts': 'control-flow-functions',
        'structs': 'data-types',
        'enums': 'data-types',
        'unions': 'data-types',
        'match': 'data-types',
        'generics': 'data-types',
        'mvs-overview': 'value-semantics',
        'mvs-modes': 'value-semantics',
        'mvs-exclusivity': 'value-semantics',
        'mvs-drop': 'value-semantics',
        'comptime': 'compile-time',
        'intrinsics': 'compile-time',
        'imports': 'modules',
        'testing': 'modules',
        'extern': 'modules'
    };
    var id = location.hash.slice(1);
    if (moved[id]) location.replace("{{ '/reference/' | relative_url }}" + moved[id] + '/#' + id);
})();
</script>
