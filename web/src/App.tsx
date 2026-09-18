import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Check, Copy, GitBranch, LogOut, Plus, ShieldCheck } from "lucide-react"
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

class Unauthorized extends Error {}

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

function Login() {
  const queryClient = useQueryClient()
  const [secret, setSecret] = useState("")
  const login = useMutation({
    mutationFn: () => api<void>("/api/v1/session", { method: "POST", body: JSON.stringify({ secret }) }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["repositories"] }),
  })

  return (
    <main className="grid min-h-screen place-items-center bg-zinc-50 p-6">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <div className="mb-3 flex h-11 w-11 items-center justify-center rounded-xl bg-zinc-950 text-white">
            <ShieldCheck size={22} />
          </div>
          <CardTitle>Open Refuge</CardTitle>
          <CardDescription>Enter the owner key configured for this server.</CardDescription>
        </CardHeader>
        <CardContent>
          <form
            className="space-y-4"
            onSubmit={(event) => {
              event.preventDefault()
              login.mutate()
            }}
          >
            <Input
              autoFocus
              type="password"
              autoComplete="current-password"
              placeholder="Owner key"
              value={secret}
              onChange={(event) => setSecret(event.target.value)}
            />
            {login.error && <p className="text-sm text-red-600">{login.error.message}</p>}
            <Button className="w-full" disabled={!secret || login.isPending}>
              {login.isPending ? "Opening…" : "Continue"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  )
}

export default function App() {
  const queryClient = useQueryClient()
  const [name, setName] = useState("")
  const [copied, setCopied] = useState<string | null>(null)
  const repositories = useQuery({
    queryKey: ["repositories"],
    queryFn: () => api<Repository[]>("/api/v1/repos"),
  })
  const create = useMutation({
    mutationFn: (repositoryName: string) =>
      api<Repository>("/api/v1/repos", {
        method: "POST",
        body: JSON.stringify({ name: repositoryName }),
      }),
    onSuccess: () => {
      setName("")
      queryClient.invalidateQueries({ queryKey: ["repositories"] })
    },
  })

  if (repositories.error instanceof Unauthorized) return <Login />

  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (name) create.mutate(name)
  }
  const items = repositories.data ?? []

  return (
    <main className="min-h-screen bg-zinc-50 text-zinc-950">
      <header className="border-b border-zinc-200 bg-white">
        <div className="mx-auto flex max-w-6xl items-center justify-between px-6 py-4">
          <div className="flex items-center gap-3 font-semibold">
            <div className="grid h-9 w-9 place-items-center rounded-lg bg-zinc-950 text-white"><ShieldCheck size={19} /></div>
            Refuge
          </div>
          <Button
            variant="ghost"
            size="sm"
            onClick={async () => {
              await api<void>("/api/v1/session", { method: "DELETE" })
              queryClient.clear()
            }}
          ><LogOut size={16} /> Log out</Button>
        </div>
      </header>

      <div className="mx-auto max-w-6xl space-y-8 px-6 py-10">
        <section>
          <p className="text-sm font-medium text-zinc-500">Git hosting and verified backups</p>
          <h1 className="mt-1 text-3xl font-semibold tracking-tight">Repositories</h1>
        </section>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">Create a repository</CardTitle>
            <CardDescription>It will be immediately available over standard Git HTTP.</CardDescription>
          </CardHeader>
          <CardContent>
            <form className="flex flex-col gap-3 sm:flex-row" onSubmit={submit}>
              <Input placeholder="notes" value={name} onChange={(event) => setName(event.target.value)} />
              <Button disabled={!name || create.isPending}><Plus size={16} /> Create</Button>
            </form>
            {create.error && <p className="mt-3 text-sm text-red-600">{create.error.message}</p>}
          </CardContent>
        </Card>

        <section className="grid gap-4">
          {repositories.isPending && <p className="text-sm text-zinc-500">Loading repositories…</p>}
          {items.length === 0 && !repositories.isPending && (
            <Card><CardContent className="py-12 text-center text-sm text-zinc-500">No repositories yet.</CardContent></Card>
          )}
          {items.map((repository) => {
            const url = `${window.location.origin}${repository.clone_path}`
            const command = `git clone ${url}`
            return (
              <Card key={repository.id}>
                <CardContent className="flex flex-col gap-5 pt-6 md:flex-row md:items-center md:justify-between">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <GitBranch size={17} className="text-zinc-500" />
                      <h2 className="font-semibold">{repository.name}</h2>
                      <Badge className={repository.protection === "protected" ? "bg-emerald-50 text-emerald-700" : "bg-amber-50 text-amber-700"}>
                        {repository.protection}
                      </Badge>
                    </div>
                    <p className="mt-2 truncate font-mono text-xs text-zinc-500">{repository.id}</p>
                  </div>
                  <div className="flex min-w-0 items-center gap-2 rounded-lg bg-zinc-100 p-2 pl-3 md:max-w-xl">
                    <code className="truncate text-xs text-zinc-700">{command}</code>
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label="Copy clone command"
                      onClick={async () => {
                        await navigator.clipboard.writeText(command)
                        setCopied(repository.id)
                        window.setTimeout(() => setCopied(null), 1500)
                      }}
                    >{copied === repository.id ? <Check size={15} /> : <Copy size={15} />}</Button>
                  </div>
                </CardContent>
              </Card>
            )
          })}
        </section>
      </div>
    </main>
  )
}
