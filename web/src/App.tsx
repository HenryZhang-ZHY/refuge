import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { AlertCircle, Check, Copy, GitBranch, LogOut, Plus, ShieldCheck } from "lucide-react"
import { type FormEvent, useState } from "react"

import { Badge } from "./components/ui/badge"
import { Button } from "./components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./components/ui/card"
import { Input } from "./components/ui/input"

type Repository = {
  name: string
  id: string
  clone_path: string
  protection: "protected" | "pending" | "unprotected" | "corrupt"
  snapshot_id: string | null
}

class Unauthorized extends Error {
  constructor() {
    super("The owner key was not accepted.")
  }
}

async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: "same-origin",
    ...init,
    headers: { "Content-Type": "application/json", ...init?.headers },
  })
  if (response.status === 401) throw new Unauthorized()
  if (!response.ok) {
    const body = await response.json().catch(() => null)
    throw new Error(body?.error?.message ?? `Request failed (${response.status})`)
  }
  return response.status === 204 ? (undefined as T) : response.json()
}

function Login({ onAuthenticated }: { onAuthenticated: () => void }) {
  const [secret, setSecret] = useState("")
  const login = useMutation({
    mutationFn: () => api<void>("/api/v1/session", { method: "POST", body: JSON.stringify({ secret }) }),
    onSuccess: onAuthenticated,
  })

  return (
    <main className="grid min-h-screen place-items-center bg-zinc-50 p-6">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <div className="mb-3 flex h-11 w-11 items-center justify-center rounded-xl bg-zinc-950 text-white">
            <ShieldCheck size={22} aria-hidden="true" />
          </div>
          <CardTitle>Open Refuge</CardTitle>
          <CardDescription>Enter the owner key configured for this server.</CardDescription>
        </CardHeader>
        <CardContent>
          <form className="space-y-4" onSubmit={(event) => { event.preventDefault(); login.mutate() }}>
            <div className="space-y-2">
              <label className="text-sm font-medium" htmlFor="owner-key">Owner key</label>
              <Input
                id="owner-key"
                autoFocus
                type="password"
                autoComplete="current-password"
                value={secret}
                aria-invalid={login.isError}
                onChange={(event) => setSecret(event.target.value)}
              />
            </div>
            {login.error && <p role="alert" className="text-sm text-red-600">{login.error.message}</p>}
            <Button className="w-full" disabled={!secret || login.isPending}>
              {login.isPending ? "Opening…" : "Continue"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  )
}

const protectionStyle = {
  protected: "bg-emerald-50 text-emerald-700",
  pending: "bg-amber-50 text-amber-700",
  unprotected: "bg-zinc-100 text-zinc-700",
  corrupt: "bg-red-50 text-red-700",
} satisfies Record<Repository["protection"], string>

export default function App() {
  const queryClient = useQueryClient()
  const [name, setName] = useState("")
  const [signedOut, setSignedOut] = useState(false)
  const [copied, setCopied] = useState<string | null>(null)
  const [copyError, setCopyError] = useState<string | null>(null)
  const repositories = useQuery({
    queryKey: ["repositories"],
    queryFn: () => api<Repository[]>("/api/v1/repos"),
    retry: false,
    refetchInterval: 5_000,
  })
  const create = useMutation({
    mutationFn: (repositoryName: string) => api<Repository>("/api/v1/repos", {
      method: "POST",
      body: JSON.stringify({ name: repositoryName }),
    }),
    onSuccess: (repository) => {
      setName("")
      queryClient.setQueryData<Repository[]>(["repositories"], (current = []) => [
        ...current.filter((item) => item.id !== repository.id),
        repository,
      ])
    },
    onError: (error) => {
      if (error instanceof Unauthorized) setSignedOut(true)
    },
  })
  const logout = useMutation({
    mutationFn: () => api<void>("/api/v1/session", { method: "DELETE" }),
    onSuccess: () => {
      queryClient.removeQueries({ queryKey: ["repositories"] })
      setSignedOut(true)
    },
  })

  const authenticated = () => {
    setSignedOut(false)
    void queryClient.resetQueries({ queryKey: ["repositories"] })
  }
  if (signedOut || repositories.error instanceof Unauthorized) {
    return <Login onAuthenticated={authenticated} />
  }

  const submit = (event: FormEvent) => {
    event.preventDefault()
    const repositoryName = name.trim()
    if (repositoryName) create.mutate(repositoryName)
  }
  const items = repositories.data ?? []

  return (
    <main className="min-h-screen bg-zinc-50 text-zinc-950">
      <header className="border-b border-zinc-200 bg-white">
        <div className="mx-auto flex max-w-6xl items-center justify-between px-6 py-4">
          <div className="flex items-center gap-3 font-semibold">
            <div className="grid h-9 w-9 place-items-center rounded-lg bg-zinc-950 text-white">
              <ShieldCheck size={19} aria-hidden="true" />
            </div>
            Refuge
          </div>
          <Button variant="ghost" size="sm" disabled={logout.isPending} onClick={() => logout.mutate()}>
            <LogOut size={16} aria-hidden="true" /> {logout.isPending ? "Logging out…" : "Log out"}
          </Button>
        </div>
      </header>

      <div className="mx-auto max-w-6xl space-y-8 px-6 py-10">
        <section>
          <p className="text-sm font-medium text-zinc-500">Git hosting and verified backups</p>
          <h1 className="mt-1 text-3xl font-semibold tracking-tight">Repositories</h1>
        </section>

        {logout.error && <p role="alert" className="text-sm text-red-600">Could not log out: {logout.error.message}</p>}

        <Card>
          <CardHeader>
            <CardTitle className="text-base">Create a repository</CardTitle>
            <CardDescription>It will be immediately available over standard Git HTTP.</CardDescription>
          </CardHeader>
          <CardContent>
            <form className="flex flex-col gap-3 sm:flex-row sm:items-end" onSubmit={submit}>
              <div className="flex-1 space-y-2">
                <label className="text-sm font-medium" htmlFor="repository-name">Repository name</label>
                <Input
                  id="repository-name"
                  placeholder="notes"
                  value={name}
                  aria-invalid={create.isError}
                  onChange={(event) => setName(event.target.value)}
                />
              </div>
              <Button aria-label="Create repository" disabled={!name.trim() || create.isPending}>
                <Plus size={16} aria-hidden="true" /> {create.isPending ? "Creating…" : "Create"}
              </Button>
            </form>
            {create.error && !(create.error instanceof Unauthorized) && (
              <p role="alert" className="mt-3 text-sm text-red-600">{create.error.message}</p>
            )}
          </CardContent>
        </Card>

        <section className="grid gap-4" aria-live="polite">
          {repositories.isPending && <p className="text-sm text-zinc-500">Loading repositories…</p>}
          {repositories.isError && (
            <Card>
              <CardContent role="alert" className="flex flex-col gap-4 py-8 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex gap-3">
                  <AlertCircle className="mt-0.5 shrink-0 text-red-600" size={18} aria-hidden="true" />
                  <div>
                    <p className="font-medium">Could not load repositories</p>
                    <p className="mt-1 text-sm text-zinc-600">{repositories.error.message}</p>
                  </div>
                </div>
                <Button variant="outline" onClick={() => void repositories.refetch()}>Retry</Button>
              </CardContent>
            </Card>
          )}
          {repositories.isSuccess && items.length === 0 && (
            <Card><CardContent className="py-12 text-center text-sm text-zinc-500">No repositories yet.</CardContent></Card>
          )}
          {items.map((repository) => {
            const command = `git clone ${window.location.origin}${repository.clone_path}`
            return (
              <Card key={repository.id}>
                <CardContent className="flex flex-col gap-5 pt-6 md:flex-row md:items-center md:justify-between">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <GitBranch size={17} className="text-zinc-500" aria-hidden="true" />
                      <h2 className="font-semibold">{repository.name}</h2>
                      <Badge className={protectionStyle[repository.protection]}>
                        {repository.protection[0].toUpperCase() + repository.protection.slice(1)}
                      </Badge>
                    </div>
                    <p className="mt-2 truncate font-mono text-xs text-zinc-500">{repository.id}</p>
                    {repository.snapshot_id && (
                      <p className="mt-1 truncate text-xs text-zinc-500">Snapshot {repository.snapshot_id}</p>
                    )}
                  </div>
                  <div className="flex min-w-0 items-center gap-2 rounded-lg bg-zinc-100 p-2 pl-3 md:max-w-xl">
                    <code className="truncate text-xs text-zinc-700">{command}</code>
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label="Copy clone command"
                      onClick={async () => {
                        try {
                          await navigator.clipboard.writeText(command)
                          setCopyError(null)
                          setCopied(repository.id)
                          window.setTimeout(() => setCopied(null), 1500)
                        } catch {
                          setCopyError(repository.id)
                        }
                      }}
                    >
                      {copied === repository.id ? <Check size={15} /> : <Copy size={15} />}
                    </Button>
                  </div>
                  {copyError === repository.id && (
                    <p role="alert" className="text-sm text-red-600">Could not copy the clone command.</p>
                  )}
                </CardContent>
              </Card>
            )
          })}
        </section>
      </div>
    </main>
  )
}
