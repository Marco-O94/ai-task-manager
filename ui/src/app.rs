//! M0 demo page: proves the vendored components render with the design tokens.
//! M1 replaces it with `AppCtx` and the real layout.

use leptos::prelude::*;

use crate::ui::badge::{Badge, BadgeVariant};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::card::{Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle};
use crate::ui::dialog::{
    Dialog, DialogAction, DialogBody, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHeader, DialogTitle, DialogTrigger,
};
use crate::ui::tabs::{Tabs, TabsContent, TabsList, TabsTrigger};

#[component]
pub fn App() -> impl IntoView {
    let dialog_open = RwSignal::new(false);

    view! {
        <main class="h-full overflow-y-auto">
            <div class="mx-auto flex max-w-4xl flex-col gap-8 p-8">
                <header class="flex flex-col gap-1">
                    <h1 class="text-2xl font-semibold">"AI Task Manager"</h1>
                    <p class="text-muted-foreground text-sm">"M0: componenti Rust/UI, token e IPC."</p>
                </header>

                <section class="flex flex-col gap-3">
                    <h2 class="font-medium">"Button"</h2>
                    <div class="flex flex-wrap gap-2">
                        <Button>"Default"</Button>
                        <Button variant=ButtonVariant::Secondary>"Secondary"</Button>
                        <Button variant=ButtonVariant::Outline>"Outline"</Button>
                        <Button variant=ButtonVariant::Ghost>"Ghost"</Button>
                        <Button variant=ButtonVariant::Destructive>"Destructive"</Button>
                        <Button variant=ButtonVariant::Success>"Success"</Button>
                        <Button variant=ButtonVariant::Warning>"Warning"</Button>
                        <Button variant=ButtonVariant::Link>"Link"</Button>
                        <Button size=ButtonSize::Sm>"Small"</Button>
                    </div>
                </section>

                <section class="flex flex-col gap-3">
                    <h2 class="font-medium">"Badge"</h2>
                    <div class="flex flex-wrap gap-2">
                        <Badge>"Default"</Badge>
                        <Badge variant=BadgeVariant::Secondary>"Da fare"</Badge>
                        <Badge variant=BadgeVariant::Info>"In corso"</Badge>
                        <Badge variant=BadgeVariant::Warning>"Richiede approvazione"</Badge>
                        <Badge variant=BadgeVariant::Success>"Fatto"</Badge>
                        <Badge variant=BadgeVariant::Destructive>"Fallito"</Badge>
                        <Badge variant=BadgeVariant::Outline>"Outline"</Badge>
                    </div>
                </section>

                <Tabs default_value="agent">
                    <TabsList>
                        <TabsTrigger value="agent">"Agente"</TabsTrigger>
                        <TabsTrigger value="changes">"Modifiche"</TabsTrigger>
                    </TabsList>
                    <TabsContent value="agent">
                        <Card>
                            <CardHeader>
                                <CardTitle>"Agente"</CardTitle>
                                <CardDescription>"Qui comparirà il transcript del turno."</CardDescription>
                            </CardHeader>
                            <CardContent>
                                <p class="text-sm">"Contenuto della card."</p>
                            </CardContent>
                            <CardFooter>
                                <Dialog open=dialog_open>
                                    <DialogTrigger>"Apri dialog"</DialogTrigger>
                                    <DialogContent class="sm:max-w-md">
                                        <DialogBody>
                                            <DialogHeader>
                                                <DialogTitle>"Dialog portato"</DialogTitle>
                                                <DialogDescription>
                                                    "Nessuno script: stato guidato da un signal. Esc o click sullo sfondo per chiudere."
                                                </DialogDescription>
                                            </DialogHeader>
                                            <DialogFooter>
                                                <DialogClose>"Annulla"</DialogClose>
                                                <DialogAction>"Conferma"</DialogAction>
                                            </DialogFooter>
                                        </DialogBody>
                                    </DialogContent>
                                </Dialog>
                            </CardFooter>
                        </Card>
                    </TabsContent>
                    <TabsContent value="changes">
                        <Card>
                            <CardHeader>
                                <CardTitle>"Modifiche"</CardTitle>
                                <CardDescription>"Qui comparirà il diff dell'attempt."</CardDescription>
                            </CardHeader>
                        </Card>
                    </TabsContent>
                </Tabs>
            </div>
        </main>
    }
}
