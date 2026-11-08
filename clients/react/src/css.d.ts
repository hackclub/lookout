// Side-effect stylesheet imports (`import "./VideoPlayer.css"`), which tsup
// inlines via injectStyle. TypeScript 6 type-checks side-effect imports, and
// this package has no bundler client types to declare them.
declare module "*.css";
