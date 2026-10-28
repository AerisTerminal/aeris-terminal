# Display mapping

`DisplayMap` projects the buffer (`Rope`) onto display rows for the textarea's
soft wrap. It is a thin facade over `TextWrapper`, which keeps one `LineItem`
per buffer line in a `SumTree` so that buffer line and display row lookups
stay logarithmic and an edit re-wraps only the lines it touched.

| Coordinate | Meaning |
|------------|---------|
| byte offset | Position in the `Rope` |
| buffer line | Logical line, split by `\n` |
| display row | One soft-wrapped segment of a buffer line |

`WrappingIndent` decides whether continuation rows keep the first row's
indentation.
