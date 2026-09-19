# Graph Report - _pbc  (2026-09-18)

## Corpus Check
- 99 files · ~295,347 words
- Verdict: corpus is large enough that graph structure adds value.
- Unclassified: 2 file(s) not represented in the graph (top: (none) 1, .ttf 1)

## Summary
- 3146 nodes · 8577 edges · 124 communities (120 shown, 4 thin omitted)
- Extraction: 96% EXTRACTED · 4% INFERRED · 0% AMBIGUOUS · INFERRED: 353 edges (avg confidence: 0.84)
- Token cost: 130,089 input · 0 output

## Community Hubs (Navigation)
- Transform Gizmo
- Render Queue UI
- Editor Behaviour Tests
- Node Graph Editor
- Vector Path Model
- FFmpeg Encoding
- Graph Wiring & Drivers
- Footage Decode Cache
- Audio Decode & Mix
- Grids & Guides
- Scene Evaluation
- Pixel Effects Rendering
- FFmpeg Decoder
- Node Kind Registry
- App Commands
- Expression Evaluator
- Graph Lowering
- Graph Raising (Round-trip)
- Vector Math Ops
- Expression IR
- Icon Font Glyphs
- App Edit Application
- Offscreen Export Render
- Effect Stack Model
- Keyframe Values & Easing
- Assets Panel
- Properties Panel
- 2.5D Camera
- Mat4 Projection
- Asset Timing
- Precomp & Camera Eval
- Curve Editor
- Audio Master Clock
- Module Params
- Animatable & Colour
- CLI Entry Point
- Bounds & Projection
- Architecture & Canvas Docs
- Driver Compilation
- Compositing & Mattes
- asset cluster
- audio cluster
- expr cluster
- prior-art cluster
- text cluster
- expr cluster
- node cluster
- dock cluster
- asset cluster
- warp cluster
- nodegraph cluster
- playback cluster
- tests cluster
- node cluster
- value cluster
- timeline cluster
- lib cluster
- raster cluster
- pathfinder cluster
- node cluster
- value cluster
- props cluster
- app cluster
- parallel cluster
- decode cluster
- node cluster
- motionpath cluster
- tests cluster
- props cluster
- expr cluster
- raster cluster
- socket cluster
- node cluster
- onion cluster
- timeline cluster
- value cluster
- timeline cluster
- tests cluster
- ? cluster
- tests cluster
- scene cluster
- tests cluster
- ? cluster
- expr cluster
- history cluster
- scene cluster
- tests cluster
- app cluster
- calc cluster
- dock cluster
- ? cluster
- ? cluster
- tests cluster
- audio cluster
- ? cluster
- expr cluster
- value cluster
- node cluster
- curves cluster
- tests cluster
- timebase cluster
- timebase cluster
- app cluster
- node cluster
- text cluster
- tests cluster
- node cluster
- tests cluster
- tests cluster
- composite cluster
- app cluster
- props cluster
- props cluster
- history cluster
- tests cluster
- tests cluster
- ? cluster
- tests cluster
- tests cluster
- tests cluster
- Cargo cluster
- ? cluster
- gotchas cluster
- performance cluster

## God Nodes (most connected - your core abstractions)
1. `NodeId` - 162 edges
2. `App` - 126 edges
3. `Node` - 75 edges
4. `CompId` - 64 edges
5. `AssetId` - 63 edges
6. `Expr` - 61 edges
7. `NodeGraph` - 59 edges
8. `ExprValue` - 58 edges
9. `EvalCtx` - 49 edges
10. `Value` - 47 edges

## Surprising Connections (you probably didn't know these)
- `recent_fonts_are_most_recently_used_and_deduplicated()` --calls--> `remember_font()`  [INFERRED]
  crates/live/src/tests.rs → crates/live/src/app.rs
- `a_new_mask_is_seeded_to_the_layers_own_size()` --calls--> `mask_seed_size()`  [INFERRED]
  crates/live/src/tests.rs → crates/live/src/app.rs
- `the_gimbal_locks_where_euler_angles_do()` --calls--> `gimbal_axes()`  [INFERRED]
  crates/live/src/tests.rs → crates/live/src/gizmo.rs
- `the_rotation_rings_are_a_gimbal()` --calls--> `gimbal_axes()`  [INFERRED]
  crates/live/src/tests.rs → crates/live/src/gizmo.rs
- `an_anchor_drag_on_a_flattened_layer_is_inert()` --calls--> `resolve_drag()`  [INFERRED]
  crates/live/src/tests.rs → crates/live/src/gizmo.rs

## Import Cycles
- 1-file cycle: `crates/core/src/expr.rs -> crates/core/src/expr.rs`
- 2-file cycle: `crates/core/src/expr.rs -> crates/core/src/node.rs -> crates/core/src/expr.rs`
- 2-file cycle: `crates/core/src/expr.rs -> crates/core/src/value.rs -> crates/core/src/expr.rs`
- 3-file cycle: `crates/core/src/expr.rs -> crates/core/src/node.rs -> crates/core/src/path.rs -> crates/core/src/expr.rs`
- 3-file cycle: `crates/core/src/expr.rs -> crates/core/src/node.rs -> crates/core/src/value.rs -> crates/core/src/expr.rs`
- 4-file cycle: `crates/core/src/expr.rs -> crates/core/src/node.rs -> crates/core/src/path.rs -> crates/core/src/value.rs -> crates/core/src/expr.rs`

## Hyperedges (group relationships)
- **Vector substrate to compositor pipeline** — docs_decisions_0004_vector_first_raster_compositor_vector_first_raster_compositor, docs_decisions_0005_compositor_stage_compositor_stage, docs_decisions_0006_25d_level_a_level_a_2_5d, docs_decisions_0013_composition_node_graph_composition_node_graph [INFERRED 0.85]
- **Everything lowers to one expression IR** — docs_architecture_expressions, docs_decisions_0010_expression_ir_expression_ir, docs_decisions_0013_composition_node_graph_composition_node_graph, docs_architecture_shared_animation_modules [INFERRED 0.85]
- **Export pipeline decisions** — docs_decisions_0017_offline_cpu_rasterizer_cpu_rasterizer, docs_decisions_0018_two_render_buttons_draft_master, docs_decisions_0020_the_render_job_is_stepped_not_threaded_stepped_render_job, docs_production_plan_phase1_export [INFERRED 0.85]
- **Pure data / headless engine principles** — docs_invariants_headless_core, docs_invariants_evaluate, docs_prior_art_lazy_value_recipe, docs_decisions_0014_undo_is_snapshots_undo_snapshots [INFERRED 0.75]

## Communities (124 total, 4 thin omitted)

### Community 0 - "Transform Gizmo"
Cohesion: 0.06
Nodes (78): ANCHOR_GRAB, ANCHOR_R, ARROW_HEAD, ARROW_LEN, axis_column(), axis_of(), BBOX_COL, BOX_HALF (+70 more)

### Community 1 - "Render Queue UI"
Cohesion: 0.05
Nodes (67): RenderBar, export_size(), a_chosen_draft_destination_can_still_collide_with_a_master(), a_chosen_path_outside_the_project_stays_absolute(), a_chosen_path_under_the_project_is_stored_relative(), a_draft_goes_next_to_the_project(), a_draft_that_would_overwrite_a_master_is_detected(), a_job_for_a_missing_comp_refuses_to_start() (+59 more)

### Community 2 - "Editor Behaviour Tests"
Cohesion: 0.03
Nodes (42): a_bare_group_gets_a_gizmo_target_from_its_place(), a_keyboard_edit_never_merges_into_the_previous_one(), a_layer_outside_its_window_has_no_place_to_hang_a_gizmo_on(), a_layer_that_draws_nothing_has_no_selection_box(), a_marquee_selects_the_nodes_its_box_covers(), a_module_knob_becomes_a_socket_on_its_links(), a_new_edit_burns_the_redo_stack(), a_new_mask_is_seeded_to_the_layers_own_size() (+34 more)

### Community 3 - "Node Graph Editor"
Cohesion: 0.08
Nodes (81): GraphCtx, GraphNode, BODY_PAD, canvas(), CFG_LINE, CFG_TOP_GAP, collect_layer_info(), config_height() (+73 more)

### Community 4 - "Vector Path Model"
Cohesion: 0.06
Nodes (57): a_corner_polyline_resolves_to_straight_segments(), Anchor, closing_adds_a_segment_and_a_close(), ctx(), from_bez_round_trips_a_cubic_contour(), PathPart, .ALL, PathSample (+49 more)

### Community 5 - "FFmpeg Encoding"
Cohesion: 0.08
Nodes (40): AsRef, a_missing_ffmpeg_is_named(), a_mov_master_is_prores_and_an_mp4_master_is_x264(), a_png_preparer_refuses_a_wrongly_sized_frame(), a_png_sequence_writes_numbered_frames(), a_real_ffmpeg_encode_writes_a_playable_file(), a_wrongly_sized_frame_is_refused(), an_aborted_ffmpeg_encode_leaves_no_file() (+32 more)

### Community 6 - "Graph Wiring & Drivers"
Cohesion: 0.11
Nodes (37): a_driver_needs_both_a_target_and_a_wire(), a_math_nodes_shape_follows_its_operator(), a_pre_node_projects_driver_lists_migrate_into_sink_nodes(), a_project_persists_its_graph_and_drivers(), a_ref_nodes_socket_follows_the_property_it_reads(), a_valid_wire_connects_and_a_type_mismatch_is_refused(), a_wire_that_would_close_a_cycle_is_refused(), an_input_takes_one_wire_and_a_second_replaces_the_first() (+29 more)

### Community 7 - "Footage Decode Cache"
Cohesion: 0.06
Nodes (43): ApplicationHandler, arc, collections, ImagePaint, BUDGET_BYTES, Cached, decode_loop(), FootageCache (+35 more)

### Community 8 - "Audio Decode & Mix"
Cohesion: 0.07
Nodes (49): audiodecoderoptions, codecparameters, a_matched_rate_read_is_exact(), a_partial_range_mixes_only_itself(), a_read_over_the_end_is_padded(), a_real_wav_decodes_to_the_right_length(), a_reversed_range_mixes_nothing(), a_silent_comp_still_mixes_the_right_length() (+41 more)

### Community 9 - "Grids & Guides"
Cohesion: 0.09
Nodes (47): Guide, GuideAxis, AidEdits, aids_ui(), comp_coord(), draw_grid(), draw_guide(), draw_rulers() (+39 more)

### Community 10 - "Scene Evaluation"
Cohesion: 0.08
Nodes (37): binop, a_blend_mode_covers_the_layers_own_artwork_not_its_children(), a_blend_mode_on_a_bare_group_does_nothing(), a_blended_child_composites_beside_its_parent_not_inside_it(), a_broken_script_warns_against_the_node_that_owns_it(), a_compound_group_merges_its_children_into_one_item(), a_document_without_blend_modes_has_no_groups(), a_group_has_no_render_item_but_still_has_a_pivot() (+29 more)

### Community 11 - "Pixel Effects Rendering"
Cohesion: 0.10
Nodes (47): color, ResolvedEffect, a_180_hue_rotation_takes_red_to_cyan(), a_drop_shadow_lands_beside_the_artwork(), a_full_tint_replaces_the_colour(), a_tiny_blur_is_a_no_op(), a_zero_tint_changes_nothing(), alpha_bounds() (+39 more)

### Community 12 - "FFmpeg Decoder"
Cohesion: 0.11
Nodes (19): DecodeError, Display, Error, a_real_ffmpeg_stream_yields_whole_frames_in_order(), extension(), ffmpeg_bin(), FfmpegDecoder, ffprobe_bin() (+11 more)

### Community 13 - "Node Kind Registry"
Cohesion: 0.11
Nodes (23): a_descriptor_with_a_repeated_socket_id_is_refused(), a_duplicate_kind_id_is_refused_not_overwritten(), a_geometry_nodes_scalar_outputs_echo_its_inputs(), a_plugin_descriptor_registers_like_a_builtin(), builtin_descriptors(), builtins_register_cleanly_and_are_findable(), by_category_filters_and_keeps_order(), generator_sockets_match_the_ir_knob_names() (+15 more)

### Community 14 - "App Commands"
Cohesion: 0.10
Nodes (35): apply_fps_edit(), AUDIO_EXTS, create_layer_from_geometry(), FOOTAGE_EXTS, footage_layer(), group_layer(), import_footage(), import_property() (+27 more)

### Community 15 - "Expression Evaluator"
Cohesion: 0.09
Nodes (18): as_scalar(), Axis, .ALL, component(), eval_expr(), eval_num(), every_waveform_stays_in_unit_range_and_hits_its_shape(), Expr (+10 more)

### Community 16 - "Graph Lowering"
Cohesion: 0.12
Nodes (39): NO_MODULES, BTreeMap, a_generator_lowers_with_its_knob_defaults(), a_graph_authored_module_evaluates_through_a_link(), a_knob_socket_is_type_checked_like_any_other(), a_math_graph_lowers_and_evaluates(), a_module_body_compiles_from_its_own_graph(), a_module_without_a_graph_output_keeps_its_body() (+31 more)

### Community 17 - "Graph Raising (Round-trip)"
Cohesion: 0.10
Nodes (37): a_concatenation_round_trips(), a_generator_with_wired_knobs_round_trips(), a_hand_drawn_path_cannot_be_raised(), a_keyframed_param_is_refused_by_name(), a_module_links_overrides_round_trip(), a_plain_module_link_round_trips(), a_rect_round_trips_through_the_canvas(), a_script_round_trips() (+29 more)

### Community 18 - "Vector Math Ops"
Cohesion: 0.08
Nodes (19): Add, AddAssign, f64, kurbo::Vec2, lerps_all_three_axes(), round_trips_through_json_with_z(), Default, Fn (+11 more)

### Community 19 - "Expression IR"
Cohesion: 0.10
Nodes (30): cell, a_bad_script_errors_but_does_not_break_the_frame(), a_generator_round_trips_through_json_and_prints_readably(), a_literal_resolves_to_itself(), a_script_evaluates_against_the_frame(), an_operator_broadcasts_a_scalar_over_a_vector(), arithmetic_composes(), arithmetic_never_produces_a_nan_or_an_infinity() (+22 more)

### Community 20 - "Icon Font Glyphs"
Cohesion: 0.05
Nodes (39): ADD, BACK, CLOSE, DELETE, EDIT, ELLIPSE, ENTER, FAMILY (+31 more)

### Community 21 - "App Edit Application"
Cohesion: 0.12
Nodes (10): App, bake_unbound(), Context, FnOnce, Instant, Renderer, prop_of_mut(), rgb_color() (+2 more)

### Community 22 - "Offscreen Export Render"
Cohesion: 0.11
Nodes (26): affine, a_blurred_layer_exports_blurred(), a_rendered_frame_carries_no_frame_border(), an_offscreen_render_comes_back_upright_and_in_rgba(), centred_square_project(), FrameRenderer, Headless, OffscreenTarget (+18 more)

### Community 23 - "Effect Stack Model"
Cohesion: 0.11
Nodes (18): a_disabled_effect_resolves_away(), a_drop_shadow_seeds_visible(), an_enabled_effect_resolves_its_params(), blur_radius_never_goes_negative(), Effect, EffectKind, EffectParamSpec, EffectType (+10 more)

### Community 24 - "Keyframe Values & Easing"
Cohesion: 0.11
Nodes (27): a_held_segment_stays_flat_then_jumps(), a_string_track_holds_each_key_until_the_next(), boxed_in_group_does_not_move(), built_in_presets_are_distinguishable(), copy_paste_preserves_spacing_values_and_easing(), deleting_a_key_from_a_pair_leaves_a_track(), deleting_a_key_that_is_not_there_changes_nothing(), deleting_the_only_key_stops_the_animation() (+19 more)

### Community 25 - "Assets Panel"
Cohesion: 0.13
Nodes (32): asset_info(), AssetDrag, assets_ui(), comp_info(), kind_icon(), row(), FnOnce, MProject (+24 more)

### Community 26 - "Properties Panel"
Cohesion: 0.13
Nodes (34): AudioInfo, CH_B, CH_G, CH_R, CH_X, CH_Y, CH_Z, Channel (+26 more)

### Community 27 - "2.5D Camera"
Cohesion: 0.13
Nodes (23): a_default_camera_matches_the_old_fixed_one(), a_layer_at_or_behind_the_eye_is_culled(), a_layer_at_zero_depth_is_untouched(), a_legacy_distance_only_camera_loads_as_an_object(), a_tipped_layer_stays_spatial(), an_identity_orbit_is_a_no_op(), cam(), Camera (+15 more)

### Community 28 - "Mat4 Projection"
Cohesion: 0.12
Nodes (14): a_direction_ignores_translation(), affine_round_trips_through_mat4(), flat_composition_stays_flat(), Mat4, .IDENTITY, Affine, Mul, Option (+6 more)

### Community 29 - "Asset Timing"
Cohesion: 0.11
Nodes (12): project_has_audio(), Asset, collect_audio(), collect_audio_node(), Comp, .DEFAULT_DURATION_FRAMES, .DEFAULT_MOTION_PATH_RANGE, .DEFAULT_PASSEPARTOUT (+4 more)

### Community 30 - "Precomp & Camera Eval"
Cohesion: 0.09
Nodes (33): a_camera_draws_far_layers_first(), a_camera_makes_depth_shrink_a_layer_toward_frame_centre(), a_dangling_precomp_reference_warns(), a_footage_layer_is_a_rect_that_names_its_source_frame(), a_layer_behind_the_eye_is_culled(), a_module_that_links_itself_warns(), a_mutual_comp_cycle_warns(), a_precomp_is_retimed_by_its_layers_local_time() (+25 more)

### Community 31 - "Curve Editor"
Cohesion: 0.11
Nodes (25): apply_tool(), curve_rows(), CurveRow, curves_ui(), CurveTool, property_column(), PropSelection, Default (+17 more)

### Community 32 - "Audio Master Clock"
Cohesion: 0.12
Nodes (19): AtomicBool, AtomicI64, a_comp_with_sound_falls_back_until_the_stream_is_live(), a_live_stream_with_no_rate_is_not_the_time_source(), AudioPosition, ClockSource, fold_into_loop(), folding_leaves_a_position_inside_its_span_alone() (+11 more)

### Community 33 - "Module Params"
Cohesion: 0.14
Nodes (26): a_cycle_is_broken_with_a_warning_not_a_hang(), a_generator_knob_can_be_a_parameter(), a_links_overrides_are_addressable_children(), a_missing_param_warns_instead_of_resolving_to_zero_silently(), a_missing_stroke_or_shape_param_resolves_neutral(), a_param_can_be_keyframed_like_any_value(), a_param_node_reads_its_own_nodes_knob(), a_param_that_reads_itself_is_caught_as_a_cycle() (+18 more)

### Community 34 - "Animatable & Colour"
Cohesion: 0.10
Nodes (16): Clone, Animatable, Color, EasePreset, .BUILT_IN, f64, Handle, .LINEAR_IN (+8 more)

### Community 35 - "CLI Entry Point"
Cohesion: 0.14
Nodes (24): an_unknown_flag_is_an_error_not_a_filename(), args(), demo(), main(), mix_soundtrack(), options_parse_into_the_render_request(), Opts, parse() (+16 more)

### Community 36 - "Bounds & Projection"
Cohesion: 0.13
Nodes (26): EYE, Projector, Point, a_groups_world_matrix_carries_the_parent_chain(), a_matte_does_not_grow_the_bounds_of_what_it_cuts(), compound_geometry(), content_bounds(), draw_order() (+18 more)

### Community 37 - "Architecture & Canvas Docs"
Cohesion: 0.10
Nodes (30): tabler-icons.ttf, Architecture, Expressions (expr.rs), Scene::places (place separate from drawing), Shared Animation Modules (Module + Expr::Use), Text as Glyph Outlines, Anchor Handle and Selection Box, Canvas Tools (+22 more)

### Community 38 - "Driver Compilation"
Cohesion: 0.10
Nodes (30): compile_drivers(), a_geometry_driver_authors_a_layers_shape(), a_geometry_node_can_create_the_layer_it_drives(), a_half_configured_sink_drives_nothing(), a_hand_made_shape_imports_onto_the_canvas_and_still_drives_its_layer(), a_property_another_driver_still_writes_is_left_alone(), a_property_driver_overrides_one_param_of_a_graph_authored_shape(), a_property_expression_imports_into_the_sink_that_drives_it() (+22 more)

### Community 39 - "Compositing & Mattes"
Cohesion: 0.09
Nodes (15): audio, ComposeMode, Mask, MatteMode, .ALL, Self, the_menu_lists_each_mode_once(), all_lists_every_variant() (+7 more)

### Community 40 - "asset cluster"
Cohesion: 0.13
Nodes (16): a_clips_duration_converts_into_comp_frames(), a_still_has_one_frame_and_no_duration(), AssetKind, AssetMeta, clip(), default_name(), footage_that_runs_out_holds_its_last_frame(), relinking_takes_the_replacements_metadata_but_keeps_identity() (+8 more)

### Community 41 - "audio cluster"
Cohesion: 0.13
Nodes (24): AssetId, a_centred_pan_is_equal_and_constant_power(), a_pan_sweep_holds_its_power(), a_partly_overlapping_source_lands_at_the_right_offset(), a_reused_buffer_is_cleared_first(), a_short_read_fills_what_it_can(), a_silent_project_yields_no_sources(), a_source_outside_the_block_is_not_even_fetched() (+16 more)

### Community 42 - "expr cluster"
Cohesion: 0.12
Nodes (12): eval_use(), EvalCtx<'a>, PropPath, .ALL, FnOnce, Into, R, String (+4 more)

### Community 43 - "prior-art cluster"
Cohesion: 0.08
Nodes (29): Undo/Redo as Whole-Document Snapshots, Dock Layout Tree with Deferred Ops, SVG Track Mattes as Luminance Masks, Offline CPU Rasterizer with Structural Parity, Two Render Buttons: Draft and Master, Console Panel (Automation API First Surface), Stepped (Not Threaded) GUI Render Job, Audio Is the Master Clock (+21 more)

### Community 44 - "text cluster"
Cohesion: 0.10
Nodes (22): Alignment, a_bigger_size_makes_a_bigger_box(), an_unknown_family_falls_back_to_something_drawable(), font_family(), BezPath, Option, Rect, String (+14 more)

### Community 45 - "expr cluster"
Cohesion: 0.11
Nodes (9): Color, f64, FromExpr, kurbo::Vec2, Fn, Self, ToExpr, Vec3 (+1 more)

### Community 46 - "node cluster"
Cohesion: 0.11
Nodes (18): a_degenerate_onion_setting_cannot_stack_or_explode(), a_degenerate_rate_does_not_move_keys_to_nowhere(), a_knobs_constant_round_trips_through_the_editor_seam(), a_lone_ghost_is_fully_solid(), a_mistyped_literal_does_not_retype_the_knob(), an_animated_knob_has_no_constant_to_show(), changing_fps_keeps_keys_at_their_wall_clock_time(), clip_windows_follow_the_new_grid() (+10 more)

### Community 47 - "dock cluster"
Cohesion: 0.15
Nodes (24): area_header(), Branch, CameraBar, COMP_H, comp_ui(), CompEntry, DockCmd, Editor (+16 more)

### Community 48 - "asset cluster"
Cohesion: 0.17
Nodes (11): Decoder, DecoderRegistry, Frame, FrameStream, Box, Formatter, Option, Path (+3 more)

### Community 49 - "warp cluster"
Cohesion: 0.18
Nodes (21): a_distant_eye_flattens_toward_orthographic(), a_path_crossing_the_eye_plane_is_refused(), a_straight_edge_stays_straight(), cubic_at(), EYE, flatten_into(), FLATTEN_TOLERANCE, Homography (+13 more)

### Community 50 - "nodegraph cluster"
Cohesion: 0.18
Nodes (19): category_tint(), col32(), draw_graph(), draw_node(), field_rect(), Color32, Default, HashMap (+11 more)

### Community 51 - "playback cluster"
Cohesion: 0.21
Nodes (20): a_missing_sound_is_silence_not_a_stall(), a_published_mix_replaces_the_previous_one_wholesale(), a_stopped_transport_is_silent_and_does_not_advance(), AudioOut, engine_with(), MixEngine, MixState, playback_wraps_within_the_loop_span() (+12 more)

### Community 52 - "tests cluster"
Cohesion: 0.15
Nodes (24): AtomicUsize, apply_effect_op(), a_first_ask_does_not_block_and_the_frame_arrives_after(), a_layer_offers_only_the_parameters_its_effects_have(), a_pending_frame_shows_its_nearest_neighbour_meanwhile(), a_stale_index_no_ops_instead_of_panicking(), adding_an_effect_appends_a_seeded_one(), an_animated_radius_actually_changes_over_time() (+16 more)

### Community 53 - "node cluster"
Cohesion: 0.11
Nodes (5): .DEFAULT_BG, LayerTiming, Node, Color, Stroke

### Community 54 - "value cluster"
Cohesion: 0.13
Nodes (5): Interp, Keyframe, pasting_onto_a_constant_starts_animating_it(), Vec, Track<T>

### Community 55 - "timeline cluster"
Cohesion: 0.15
Nodes (21): setting_a_work_edge_seeds_the_other_from_the_comp(), work_area_loop_bounds_stay_inside_the_comp(), clip_grab_at(), ClipGrab, ClipInfo, DOPESHEET_H, EDGE_PAN_W, loop_bounds() (+13 more)

### Community 56 - "lib cluster"
Cohesion: 0.21
Nodes (21): a_blend_mode_survives_into_the_svg(), a_mask_becomes_a_clip_path(), a_partly_transparent_matte_carries_its_coverage(), a_track_matte_becomes_a_mask(), an_alpha_matte_paints_white_on_black(), an_inverted_matte_subtracts_from_a_white_ground(), an_unblended_document_emits_no_groups(), blended() (+13 more)

### Community 57 - "raster cluster"
Cohesion: 0.21
Nodes (20): average_frames, a_blend_mode_reaches_the_pixels(), a_blur_spreads_past_the_shapes_edge(), a_disabled_effect_changes_nothing(), a_filled_shape_lands_on_the_canvas(), a_moving_box_smears_with_motion_blur(), a_track_matte_cuts_the_content_it_covers(), an_effect_reaches_the_rasterized_pixels() (+12 more)

### Community 58 - "pathfinder cluster"
Cohesion: 0.19
Nodes (19): Coord2, ACCURACY, BoolOp, .ALL, c(), combine(), disjoint_shapes_union_to_two_contours(), from_flo() (+11 more)

### Community 59 - "node cluster"
Cohesion: 0.23
Nodes (11): a_comp_without_a_background_loads_the_default_one(), a_comp_without_a_passepartout_loads_the_default_one(), a_comp_without_aids_loads_the_defaults_and_guides_round_trip(), a_pre_2_5d_transform_loads_flat_and_unchanged(), a_project_without_presets_loads_with_none(), changing_the_rate_keeps_the_comp_the_same_length_in_seconds(), ease_library_round_trips_and_defaults_empty_on_an_old_file(), only_out_of_plane_channels_escalate_to_a_matrix() (+3 more)

### Community 60 - "value cluster"
Cohesion: 0.14
Nodes (10): const_resolves_anywhere(), insert_key_promotes_constant_then_adds(), Keyframe<T>, Fn, FnOnce, T, set_at_overwrites_a_constant(), set_at_replaces_existing_key_and_inserts_new() (+2 more)

### Community 61 - "props cluster"
Cohesion: 0.14
Nodes (9): effect_value(), prop_of(), PropRef, PropRefMut, MColor, Option, Vec2, Vec3 (+1 more)

### Community 62 - "app cluster"
Cohesion: 0.19
Nodes (3): ActiveEventLoop, WindowEvent, WindowId

### Community 63 - "parallel cluster"
Cohesion: 0.14
Nodes (17): atomic, a_failing_frame_stops_the_render(), a_failing_sink_stops_the_render(), a_single_frame_range_works(), an_empty_range_does_nothing(), every_frame_is_rendered_exactly_once(), frames_arrive_in_order_however_they_finish(), more_threads_than_frames_is_fine() (+9 more)

### Community 64 - "decode cluster"
Cohesion: 0.12
Nodes (16): BufReader, ChildStdout, command, a_missing_frame_count_falls_back_to_duration(), a_rational_frame_rate_is_kept_as_a_ratio(), default_registry(), FfmpegStream, HEIC_EXTS (+8 more)

### Community 65 - "node cluster"
Cohesion: 0.13
Nodes (12): Grid, .DEFAULT_SPACING, .MAX_SPACING, .MIN_SPACING, Guides, MotionBlur, Onion, .MAX_GHOSTS (+4 more)

### Community 66 - "motionpath cluster"
Cohesion: 0.18
Nodes (16): DOT_COL, draw(), KEY_COL, key_indices(), MAX_RANGE, MotionPath, PATH_COL, PathKey (+8 more)

### Community 67 - "tests cluster"
Cohesion: 0.11
Nodes (19): demo_document(), Document, main(), a_bare_document_file_still_loads(), closing_an_area_keeps_the_canvas(), composite_events(), locked_layers_are_skipped_by_picking(), path_to() (+11 more)

### Community 68 - "props cluster"
Cohesion: 0.18
Nodes (12): effect_nums(), effect_params(), footage_info(), is_anim(), prop_kinds_of(), BTreeMap, Document, Fn (+4 more)

### Community 69 - "expr cluster"
Cohesion: 0.12
Nodes (9): BinOp, .ALL, collect_named(), finite_or_zero(), MathOp, Default, Vec, UnOp (+1 more)

### Community 70 - "raster cluster"
Cohesion: 0.21
Nodes (17): apply_effects(), BLACK, draw_group(), draw_item(), draw_range(), rasterize(), Affine, BezPath (+9 more)

### Community 71 - "socket cluster"
Cohesion: 0.19
Nodes (14): ExprValue, String, neutral_for(), Into, Option, Self, String, Socket (+6 more)

### Community 72 - "node cluster"
Cohesion: 0.22
Nodes (7): Module, Param, ParamValue, RenderPreset, Into, String, preset()

### Community 73 - "onion cluster"
Cohesion: 0.18
Nodes (15): collect_ids(), FUTURE, Ghost, OnionKey, OnionSkins, PAST, MColor, MNode (+7 more)

### Community 74 - "timeline cluster"
Cohesion: 0.30
Nodes (11): Axis, dopesheet_ui(), label_cell(), label_text(), Rect, Self, Ui, time_ruler() (+3 more)

### Community 75 - "value cluster"
Cohesion: 0.17
Nodes (6): legacy_seconds_migrate_to_frames(), migration_collapses_keys_that_round_onto_one_frame(), relocking_tangents_mirrors_the_out_handle(), Option, segment_handles_round_trip(), Value<T>

### Community 76 - "timeline cluster"
Cohesion: 0.14
Nodes (14): keying_a_level_makes_it_a_dopesheet_row(), selection_groups_into_one_bucket_per_property(), ClipTrack, dope_rows(), DopeRow, group_selection_by_prop(), KeyClipboard, KeySelection (+6 more)

### Community 77 - "tests cluster"
Cohesion: 0.14
Nodes (17): a_layer_snaps_to_another_layers_edge(), a_layer_with_no_geometry_snaps_only_its_pivot(), a_layers_edge_snaps_to_a_guide_not_just_its_pivot(), a_nested_layer_snaps_to_the_guide_in_composition_space(), aids_with(), an_axis_constrained_drag_only_snaps_along_its_axis(), dimmed(), hidden_aids_do_not_snap() (+9 more)

### Community 78 - "? cluster"
Cohesion: 0.15
Nodes (13): keep_position(), remap(), Affine, Fn, HashMap, ImageData, MScene, Point (+5 more)

### Community 79 - "tests cluster"
Cohesion: 0.16
Nodes (16): tree_rows(), MNode, strip_rows(), a_groups_children_are_listed_front_most_first_too(), a_precomp_row_shows_the_comp_icon_over_its_shape(), a_strip_carries_its_layers_window_and_keys(), a_strip_carries_its_sound_so_the_row_can_draw_a_waveform(), a_strips_keys_are_deduped_across_properties() (+8 more)

### Community 80 - "scene cluster"
Cohesion: 0.21
Nodes (15): canvas_scale(), canvas_transform(), CanvasNav, .MAX_PITCH, fit_area(), fit_transform(), nav_zoom_about(), Default (+7 more)

### Community 81 - "tests cluster"
Cohesion: 0.17
Nodes (8): FakeDecoder, FakeStream, layer_rows_pick_an_icon_per_shape(), optional_properties_are_absent_when_the_node_lacks_them(), Box, Option, Path, Result

### Community 82 - "? cluster"
Cohesion: 0.15
Nodes (13): crate, HANDLE_W, Axis, Painter, Rect, waveform(), install(), PREVIEW_BACKDROP (+5 more)

### Community 83 - "expr cluster"
Cohesion: 0.30
Nodes (14): build_engine(), dynamic_to_expr_value(), dynamic_to_num(), expr_value_to_dynamic(), Box, Result, script_err(), script_param() (+6 more)

### Community 84 - "history cluster"
Cohesion: 0.23
Nodes (7): History, Into, MProject, Option, String, Vec, Step

### Community 85 - "scene cluster"
Cohesion: 0.17
Nodes (14): CANVAS_BAR_H, canvas_toolbar(), CanvasEdits, Chrome, FIT_MARGIN, group_bounds(), MAX_SCALE, MIN_SCALE (+6 more)

### Community 86 - "tests cluster"
Cohesion: 0.13
Nodes (15): a_color_clip_will_not_paste_onto_a_scalar_property(), a_stale_channel_index_edits_nothing(), a_string_key_clip_only_pastes_onto_a_string_property(), a_text_layers_content_is_an_animatable_property(), a_vector_layer_exposes_a_property_per_anchor_control_point(), a_vector_property_plots_one_curve_per_axis(), animating_one_point_gives_it_a_dopesheet_row_and_leaves_the_rest_alone(), dope_rows_lists_animated_shape_and_stroke_properties() (+7 more)

### Community 88 - "calc cluster"
Cohesion: 0.28
Nodes (6): calc(), num(), Parser, Option, DragValue, N

### Community 89 - "dock cluster"
Cohesion: 0.23
Nodes (6): Dock, DockSide, Box, count_editor(), dock_editors(), innermost_is_canvas()

### Community 90 - "? cluster"
Cohesion: 0.18
Nodes (11): Chrome<'static>, rasterize_effect_layers(), read_texture_rgba(), Device, HashMap, ImageData, Queue, Renderer (+3 more)

### Community 91 - "? cluster"
Cohesion: 0.17
Nodes (13): Arc, HashMap, Option, String, Ui, Vec, StripRow, strips_ui() (+5 more)

### Community 92 - "tests cluster"
Cohesion: 0.15
Nodes (13): a_flat_segment_keeps_its_tangent_height(), a_held_segment_shows_no_tangent(), axis_round_trips_frames_through_pixels(), axis_round_trips_when_panned_and_zoomed(), curve_axis(), dragging_a_broken_tangent_leaves_the_other_side_put(), dragging_a_locked_tangent_moves_both_sides(), Axis (+5 more)

### Community 93 - "audio cluster"
Cohesion: 0.39
Nodes (11): a_cyclic_precomp_terminates(), a_head_trim_starts_partway_into_the_source(), a_muted_layer_contributes_nothing(), a_precomp_carries_its_sound_shifted_by_the_instance(), a_trimmed_layer_sounds_over_its_trim(), a_zero_sample_rate_yields_nothing(), comp_with(), level_and_pan_reach_the_source_as_gains() (+3 more)

### Community 94 - "? cluster"
Cohesion: 0.32
Nodes (12): RenderItem, Color, draw_footage(), draw_item_raw(), Affine, BTreeMap, Color, MColor (+4 more)

### Community 95 - "expr cluster"
Cohesion: 0.18
Nodes (7): a_bad_name_or_property_is_a_script_error(), enter(), eval_script_ctx(), Guard, Drop, TimeSource, wiggle_is_continuous_across_a_lattice_point()

### Community 96 - "value cluster"
Cohesion: 0.23
Nodes (8): EvalCtx, ResolveCache, BTreeMap, HashMap, HashSet, at(), vec2_track(), PropKey

### Community 97 - "node cluster"
Cohesion: 0.23
Nodes (9): de_rotation(), de_text_content(), Error, Result, Vec3, Transform, widen_rotation(), zero_rotation() (+1 more)

### Community 98 - "curves cluster"
Cohesion: 0.24
Nodes (10): mirror_handle(), drag_tangent(), GRAB_R, Axis, Fn, Option, Pos2, SAMPLE_PX (+2 more)

### Community 99 - "tests cluster"
Cohesion: 0.30
Nodes (12): a_long_drag_does_not_compound_rounding(), a_second_drag_starts_from_the_committed_rate(), a_selection_follows_keys_that_merge(), a_stale_selection_entry_is_dropped(), a_typed_fps_retimes_without_a_drag(), comp_at_fps(), dragging_the_fps_spinner_retimes_on_every_delta(), fps_edit() (+4 more)

### Community 100 - "timebase cluster"
Cohesion: 0.31
Nodes (8): degenerate_fps_does_not_poison_conversions(), fractional_fps_uses_nominal_for_timecode(), negative_positions_format_with_a_sign(), Self, seconds_round_trip_through_frames(), seconds_to_frames_rounds_to_nearest(), timecode_fields_roll_over(), timecode_frame_field_never_reaches_fps()

### Community 101 - "timebase cluster"
Cohesion: 0.22
Nodes (3): Default, String, Timebase

### Community 102 - "app cluster"
Cohesion: 0.29
Nodes (4): increment_path(), is_audio_path(), Path, PathBuf

### Community 103 - "node cluster"
Cohesion: 0.28
Nodes (4): a_comp_with_no_duration_at_all_migrates_to_the_default_length(), a_legacy_seconds_duration_migrates_to_frames(), a_migrated_comp_no_longer_serializes_the_legacy_field(), legacy_json()

### Community 104 - "text cluster"
Cohesion: 0.33
Nodes (3): PathPen, Point, OutlinePen

### Community 105 - "tests cluster"
Cohesion: 0.22
Nodes (9): a_blended_child_opens_its_own_layer_beside_its_parents(), a_groups_clip_bounds_include_the_stroke_width(), a_masked_layer_opens_its_own_composited_layer(), a_matted_pair_composites_as_one_isolated_unit(), an_unblended_layer_costs_no_offscreen_target(), blended_box(), precomp_layers_nest_and_balance(), MBlendMode (+1 more)

### Community 106 - "node cluster"
Cohesion: 0.29
Nodes (3): BezPath, Vec2, Shape

### Community 107 - "tests cluster"
Cohesion: 0.39
Nodes (8): split_shape(), a_group_has_no_shape_to_split_and_says_so(), project_with_hybrid_layer(), splitting_does_not_move_the_transform(), splitting_leaves_a_transform_driver_on_the_parent(), splitting_moves_the_artwork_into_a_child_and_leaves_a_group(), splitting_repoints_a_driver_at_the_moved_property(), the_new_layer_lands_where_the_shape_used_to_draw()

### Community 108 - "tests cluster"
Cohesion: 0.32
Nodes (8): a_modules_override_socket_shows_no_inline_field_while_inheriting(), an_imported_clip_gets_a_layer_window_its_own_length(), an_imported_still_gets_no_layer_window(), clip_meta(), importing_footage_sizes_the_layer_to_its_native_pixels(), seed(), the_properties_panel_reports_the_selected_layers_knobs(), time_remap_is_a_property_only_once_it_is_enabled()

### Community 109 - "composite cluster"
Cohesion: 0.29
Nodes (5): BlendMode, .ALL, css_blend(), sk_blend(), SkBlend

### Community 111 - "props cluster"
Cohesion: 0.33
Nodes (5): set_effect_num(), effect_value_mut(), EffectInfo, EffectNum, EffectParam

### Community 112 - "props cluster"
Cohesion: 0.33
Nodes (4): Shows, PropKind, .ALL, Cow

### Community 113 - "history cluster"
Cohesion: 0.40
Nodes (5): CompEdits, FileCmd, edit_label(), HistoryCmd, MAX_STEPS

### Community 114 - "tests cluster"
Cohesion: 0.33
Nodes (6): a_clip_may_extend_past_the_comp_end(), clip_drags_clamp_instead_of_inverting(), sliding_moves_the_whole_clip_so_it_plays_the_same_content_later(), slipping_moves_only_the_start(), trimming_moves_one_edge_and_leaves_the_content_put(), drag_clip()

### Community 115 - "tests cluster"
Cohesion: 0.40
Nodes (6): Context, test_ctx(), the_composition_bar_fits_its_fixed_height(), the_composition_bar_still_fits_while_a_render_runs(), wide_input(), RawInput

### Community 116 - "? cluster"
Cohesion: 0.33
Nodes (5): RasterError, Display, Error, Formatter, Result

### Community 117 - "tests cluster"
Cohesion: 0.40
Nodes (5): builtin_presets(), Preset, every_builtin_preset_is_a_valid_layout(), is_valid_accepts_layouts_and_rejects_broken_ones(), presets_offer_more_than_one_arrangement()

### Community 118 - "tests cluster"
Cohesion: 0.40
Nodes (5): a_precomp_inherits_the_open_comps_format(), precomposing_does_not_double_the_layers_transform(), precomposing_replaces_the_layer_in_place_with_an_instance(), the_root_cannot_be_precomposed(), three_layer_comp()

### Community 119 - "tests cluster"
Cohesion: 0.50
Nodes (4): passepartout_path(), BezPath, a_comp_larger_than_the_canvas_does_not_invert_the_passepartout(), the_passepartout_covers_the_canvas_and_spares_the_comp()

### Community 120 - "Cargo cluster"
Cohesion: 0.83
Nodes (4): motion-app, motion-core, motion-live, motion-render

### Community 121 - "? cluster"
Cohesion: 0.67
Nodes (3): MBlendMode, to_peniko_blend(), MComposeMode

## Knowledge Gaps
- **156 isolated node(s):** `USAGE`, `FALLBACK_DISTANCE`, `EPSILON`, `.ALL`, `.ALL` (+151 more)
  These have ≤1 connection - possible missing edges or undocumented components. (Counts symbols only; 604 node(s) total have ≤1 connection when file, concept and rationale nodes are included.)
- **4 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `NodeId` connect `Module Params` to `Transform Gizmo`, `Editor Behaviour Tests`, `Node Graph Editor`, `Vector Path Model`, `Graph Wiring & Drivers`, `Scene Evaluation`, `App Commands`, `Expression Evaluator`, `Graph Lowering`, `Graph Raising (Round-trip)`, `Expression IR`, `App Edit Application`, `Assets Panel`, `Properties Panel`, `Precomp & Camera Eval`, `Curve Editor`, `Bounds & Projection`, `Driver Compilation`, `asset cluster`, `expr cluster`, `node cluster`, `node cluster`, `node cluster`, `motionpath cluster`, `tests cluster`, `expr cluster`, `node cluster`, `onion cluster`, `? cluster`, `tests cluster`, `expr cluster`, `scene cluster`, `? cluster`, `? cluster`, `? cluster`, `value cluster`, `tests cluster`, `tests cluster`, `app cluster`, `tests cluster`?**
  _High betweenness centrality (0.117) - this node is a cross-community bridge._
- **Why does `App` connect `App Edit Application` to `Transform Gizmo`, `Render Queue UI`, `Node Graph Editor`, `Vector Path Model`, `Footage Decode Cache`, `Audio Decode & Mix`, `Grids & Guides`, `Node Kind Registry`, `App Commands`, `Asset Timing`, `Curve Editor`, `Audio Master Clock`, `Module Params`, `audio cluster`, `playback cluster`, `timeline cluster`, `app cluster`, `motionpath cluster`, `onion cluster`, `timeline cluster`, `timeline cluster`, `? cluster`, `scene cluster`, `history cluster`, `app cluster`, `dock cluster`, `app cluster`, `app cluster`, `tests cluster`?**
  _High betweenness centrality (0.085) - this node is a cross-community bridge._
- **Why does `Expr` connect `Expression Evaluator` to `Vector Path Model`, `Graph Lowering`, `Graph Raising (Round-trip)`, `Expression IR`, `Keyframe Values & Easing`, `Module Params`, `Animatable & Colour`, `expr cluster`, `node cluster`, `node cluster`, `props cluster`, `expr cluster`, `raster cluster`, `socket cluster`, `node cluster`, `value cluster`, `expr cluster`, `expr cluster`, `value cluster`, `node cluster`?**
  _High betweenness centrality (0.050) - this node is a cross-community bridge._
- **Are the 3 inferred relationships involving `NodeId` (e.g. with `a_ref_lowers_to_an_expr_ref()` and `math_round_trips()`) actually correct?**
  _`NodeId` has 3 INFERRED edges - model-reasoned connections that need verification._
- **Are the 6 inferred relationships involving `AssetId` (e.g. with `a_footage_layer_is_a_rect_that_names_its_source_frame()` and `footage_missing_from_the_project_warns_rather_than_vanishing()`) actually correct?**
  _`AssetId` has 6 INFERRED edges - model-reasoned connections that need verification._
- **What connects `USAGE`, `FALLBACK_DISTANCE`, `EPSILON` to the rest of the system?**
  _156 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Transform Gizmo` be split into smaller, more focused modules?**
  _Cohesion score 0.056100981767180924 - nodes in this community are weakly interconnected._