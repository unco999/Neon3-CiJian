# NUI Flow DataGrid And Virtual Lists

This document describes the declarative DataGrid and virtual-list features in
NUI Flow V1.

## Basic Declaration

```text
surface asset-list column w 640 h 360 gap 8 pad 12 align stretch fill #17201E
  text title h 24 value "Assets"
  data_grid assets h 300 capacity 32 row_height 28 overscan 4 columns "name:240,status:140,owner:180"
```

`capacity` is the maximum number of rows in one domain frame. It is not the
total number of rows. `overscan` keeps extra rows around the visible range so
small scroll movements do not immediately request another window.

The old `key:width` column form remains valid and creates a text column.

## Cell Presentations

Columns can declare a default presentation:

```text
data_grid assets h 300 capacity 32 row_height 28 overscan 4 columns "name:240:text,selected:90:select:asset.selected.set,status:160:dropdown:draft|ready|archived:asset.status.set,notes:260:edit:128:asset.notes.commit"
```

Supported forms:

```text
key:width:text
key:width:select:intent
key:width:dropdown:option1|option2:intent
key:width:edit:max_chars:intent
```

Rules:

- `text` is read-only display text.
- `select` requires a boolean cell value.
- `dropdown` requires an enum cell value and a declared option list.
- `edit` requires a text-handle cell value and a positive character limit.
- Interactive presentations require a dotted semantic intent.
- Options must be non-empty and unique.

The domain may override a cell presentation inside the bounded frame:

```json
{
  "presentation_override": {
    "kind": "dropdown",
    "options": ["ready"]
  }
}
```

Overrides can make a cell read-only, narrow a dropdown option list, or lower an
edit limit. They cannot introduce a new undeclared intent or an option that the
column did not declare.

## Domain Frame

NUI does not contain row data. The domain returns a revisioned bounded frame:

```text
total_rows = 10000
first_row = 480
window_rows = 32 rows
```

Each row has a stable `stable_row_key`. Each cell has a typed value and a
domain-owned display text handle. The UI runtime validates column coverage,
typed values, stable keys, list revision, and window bounds before attaching the
frame.

The renderer never materializes all rows. It draws only the current window.

## Virtual Window Requests

Scrolling is split into two paths:

1. WGPU handles wheel, thumb drag, horizontal movement, and middle-button pan
   locally at frame rate.
2. When the visible logical range leaves the current bounded frame, WGPU sends
   a latest-value `ui.data_grid.window.request`.

Requests contain revisioned semantic identity, the requested first row, the
maximum window size, and a sequence. They do not contain pointer coordinates,
hit IDs, GPU handles, or renderer paths.

Requests are coalesced and debounced. A fast wheel gesture does not create one
domain request per raw wheel event. Newer requests replace queued older ones.
The domain returns a replacement `UiDataGridFrame`, normally with the same
program and list revision and a new bounded row range.

## Scrolling

For a normal `scroll` container:

```text
scroll inspector column h 240 gap 6 pad 8 align stretch fill #22302D
  text name value "Material"
  text category value "Surface"
```

The runtime provides:

- vertical and horizontal overflow metrics;
- vertical and horizontal tracks and thumbs;
- mouse-wheel scrolling;
- Shift plus wheel for horizontal scrolling;
- middle-button two-axis panning;
- clipping of descendants to the viewport.

DataGrid uses the same local scrolling implementation, while its logical row
position is supplied by `first_row` in the domain frame.

## Cell Events

Cell interactions produce declared semantic intents:

- select emits a typed boolean value;
- dropdown emits a typed enum value;
- edit enters on the declared edit interaction and commits a bounded text
  handle.

The event target uses the grid key, stable row key, and column key. Generated
renderer paths and pointer coordinates are not public identity. The UI runtime
rejects events for rows outside the currently attached bounded frame or for
cells whose declared presentation does not allow that event.

The domain remains responsible for accepting or rejecting the mutation. A UI
event is a request, not a direct project write.

## Ownership Rules

- NUI declares topology, dimensions, columns, presentations, and intents.
- The domain owns row count, row identity, typed cell values, options, and
  revisioned window data.
- WGPU owns pixels, local scrolling, clipping, hit testing, focus, and pointer
  capture.
- The UI runtime validates frames and forwards typed semantic events.
- NUI cannot execute code, query data, format domain values, or create GPU
  resources.

## Recommended Capacity

Choose capacity from the viewport and row height. A practical starting point is
the number of visible rows plus two to four overscan rows. Larger capacities
reduce refill frequency but increase text and control work per frame.

For a 300 logical-pixel viewport with 28-pixel rows:

```text
visible rows: about 11
capacity: 24 or 32
overscan: 3 or 4
```

The correct value depends on cell complexity and measured frame time.
