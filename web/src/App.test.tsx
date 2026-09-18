import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { cleanup, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"

import App from "./App"

type FetchHandler = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  })
}

function renderApp(handler: FetchHandler) {
  vi.stubGlobal("fetch", vi.fn(handler))
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>,
  )
}

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe("Refuge dashboard", () => {
  it("shows a retryable error instead of an empty repository list", async () => {
    let attempts = 0
    renderApp(async () => {
      attempts += 1
      return attempts === 1
        ? json({ error: { message: "store unavailable" } }, 500)
        : json([])
    })

    expect(await screen.findByRole("alert")).toHaveTextContent("store unavailable")
    expect(screen.queryByText("No repositories yet.")).not.toBeInTheDocument()

    await userEvent.click(screen.getByRole("button", { name: "Retry" }))
    expect(await screen.findByText("No repositories yet.")).toBeInTheDocument()
  })

  it("signs in and returns to the repository dashboard", async () => {
    let authenticated = false
    renderApp(async (input, init) => {
      const path = String(input)
      if (path === "/api/v1/session" && init?.method === "POST") {
        authenticated = true
        return new Response(null, { status: 204 })
      }
      return authenticated ? json([]) : json({ error: { message: "unauthorized" } }, 401)
    })

    await userEvent.type(await screen.findByLabelText("Owner key"), "owner-key")
    await userEvent.click(screen.getByRole("button", { name: "Continue" }))

    expect(await screen.findByRole("heading", { name: "Repositories" })).toBeInTheDocument()
  })

  it("trims a new repository name and displays the created repository", async () => {
    const created = {
      name: "notes",
      id: "0199d840-8840-7000-8000-000000000001",
      clone_path: "/git/notes.git",
      protection: "protected",
      snapshot_id: "snapshot-1",
    }
    let repositories: typeof created[] = []
    const requests: string[] = []
    renderApp(async (input, init) => {
      if (init?.method === "POST") {
        requests.push(String(init.body))
        repositories = [created]
        return json(created, 201)
      }
      return json(repositories)
    })

    await userEvent.type(await screen.findByLabelText("Repository name"), "  notes  ")
    await userEvent.click(screen.getByRole("button", { name: "Create repository" }))

    expect(requests).toEqual([JSON.stringify({ name: "notes" })])
    expect(await screen.findByRole("heading", { name: "notes" })).toBeInTheDocument()
    expect(screen.getByText("Protected")).toBeInTheDocument()
  })
})
