import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import App from "./App";
import { invoke } from "@tauri-apps/api/core";
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  isTauri: () => true,
}));
vi.mock("./components/GeneralPlanWorkspace", () => ({
  default: ({ initialProfile }: { initialProfile: string }) => (
    <p>Active review profile: {initialProfile}</p>
  ),
}));
afterEach(cleanup);
test("live explorer and review plan remain usable when the independent example plan fails", async () => {
  vi.mocked(invoke).mockRejectedValue("Example unavailable");
  render(<App />);
  expect(screen.getByRole("button", { name: "Choose folder" })).toBeEnabled();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("get_example_plan"));
  expect(screen.getByRole("button", { name: "Choose folder" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: /review plan/i }));
  expect(await screen.findByText("Active review profile: live")).toBeVisible();
});
