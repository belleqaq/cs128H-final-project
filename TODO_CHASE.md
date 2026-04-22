# 追逐逻辑待改事项

地图生成完成后处理。

## 1. ~~仇恨时间与搜索时间取 max~~ ✅

`enter_search` 进入搜索时，`timer = max(timer, room_search_time)`。
`room_search_time = max(MIN_ROOM_SEARCH_S=3.0, tiles * SEARCH_TIME_PER_TILE=0.12)`。

## 2. ~~搜索中途发现门 → 提前跳房间~~ ✅

`find_escape_direction_door` 每 tick 检查：
- 门在 chase_dir 方向（dot > DOOR_ESCAPE_DIR_DOT=0.3）
- NPC 已看到门附近瓦片（discovered）
- 目标房间未搜过 且 searched_rooms < 2
满足则中断搜索，enter_navigate 到该房间。多个门时取 dot 最大的。
