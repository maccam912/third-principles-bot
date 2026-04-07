# Safe Collection And Placement Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent the bot from choosing collection or placement positions that strand it or block its own build target.

**Architecture:** Add pure helper functions in `src/state.rs` that identify safe adjacent stance positions and rank candidates conservatively before the runtime pathfinder commits to them. Thread those helpers into collection target selection and build placement so the bot only mines or places from supported neighboring tiles and can keep returning toward the active build origin.

**Tech Stack:** Rust, Azalea pathfinding, unit tests in `src/state.rs`

---

## Chunk 1: Pure Selection Helpers

### Task 1: Safe stance tests and implementation

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Write failing tests**
- [ ] **Step 2: Run targeted test command and verify failure**
- [ ] **Step 3: Implement minimal safe stance helpers**
- [ ] **Step 4: Run targeted test command and verify pass**
- [ ] **Step 5: Commit**

## Chunk 2: Collection Integration

### Task 2: Conservative collect target selection

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Write failing tests for risky target rejection**
- [ ] **Step 2: Run targeted test command and verify failure**
- [ ] **Step 3: Integrate safe stance checks into collection search**
- [ ] **Step 4: Run targeted test command and verify pass**
- [ ] **Step 5: Commit**

## Chunk 3: Build Placement Integration

### Task 3: Adjacent placement stance enforcement

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Write failing tests for placement stance choice**
- [ ] **Step 2: Run targeted test command and verify failure**
- [ ] **Step 3: Integrate stance selection into block placement**
- [ ] **Step 4: Run targeted test command and verify pass**
- [ ] **Step 5: Commit**

## Chunk 4: Verification

### Task 4: Regression check

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Run `cargo test`**
- [ ] **Step 2: Run `cargo clippy --all-targets --all-features -D warnings`**
- [ ] **Step 3: Commit**
