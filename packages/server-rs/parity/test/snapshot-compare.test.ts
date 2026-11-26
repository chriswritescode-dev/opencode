import { expect, test } from "bun:test"
import {
  compareSnapshotOutputs,
  formatSnapshotComparison,
  snapshotComparisonStatus,
} from "../../script/snapshot-parity"

test("identical outputs have no divergence", () => {
  const left = {
    alpha: [{ capture: "t0", entries: ["a.txt"] }, { files: ["t0", "t1"] }],
    beta: [{ capture: "t1" }],
  }
  const right = {
    alpha: [{ capture: "t0", entries: ["a.txt"] }, { files: ["t0", "t1"] }],
    beta: [{ capture: "t1" }],
  }
  const result = compareSnapshotOutputs(left, right)
  expect(result.identical).toBe(true)
  expect(result.same).toEqual(["alpha", "beta"])
  expect(result.different).toEqual([])
  expect(formatSnapshotComparison(result).at(-1)).toBe("all scenarios identical")
  expect(snapshotComparisonStatus(result)).toBe(0)
})

test("one changed step reports both values", () => {
  const left = { alpha: [{ capture: "t0" }, { capture: "t1" }] }
  const right = { alpha: [{ capture: "t0" }, { capture: "t2" }] }
  const result = compareSnapshotOutputs(left, right)
  expect(result.identical).toBe(false)
  expect(result.same).toEqual([])
  expect(result.different).toHaveLength(1)
  expect(result.different[0]!.name).toBe("alpha")
  expect(result.different[0]!.steps).toEqual([{ index: 1, left: { capture: "t1" }, right: { capture: "t2" } }])
  const lines = formatSnapshotComparison(result)
  expect(lines).toContain("DIFFERENT alpha")
  expect(lines).toContain('    ts:   {"capture":"t1"}')
  expect(lines).toContain('    rust: {"capture":"t2"}')
  expect(lines.at(-1)).toBe("1 divergent scenarios")
  expect(snapshotComparisonStatus(result)).toBe(1)
})

test("a scenario present on one side differs for every step of the longer array", () => {
  const result = compareSnapshotOutputs({ alpha: [{ capture: "t0" }, { capture: "t1" }] }, {})
  expect(result.identical).toBe(false)
  expect(result.different).toHaveLength(1)
  expect(result.different[0]!.name).toBe("alpha")
  expect(result.different[0]!.steps.map((step) => step.index)).toEqual([0, 1])
  expect(result.different[0]!.steps.map((step) => step.right)).toEqual([undefined, undefined])
  expect(snapshotComparisonStatus(result)).toBe(1)
})

test("only the equal scenario is reported same", () => {
  const result = compareSnapshotOutputs(
    { alpha: [{ capture: "t0" }], beta: [{ capture: "t0" }, { capture: "t1" }] },
    { alpha: [{ capture: "t0" }], beta: [{ capture: "t0" }, { capture: "other" }] },
  )
  expect(result.same).toEqual(["alpha"])
  expect(result.different.map((scenario) => scenario.name)).toEqual(["beta"])
  expect(formatSnapshotComparison(result).at(-1)).toBe("1 divergent scenarios")
  expect(snapshotComparisonStatus(result)).toBe(1)
})
