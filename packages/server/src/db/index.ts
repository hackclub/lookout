import { drizzle } from "drizzle-orm/node-postgres";
import pg from "pg";
import * as schema from "./schema.js";

const DATABASE_URL = process.env.DATABASE_URL;
if (!DATABASE_URL) {
  throw new Error("DATABASE_URL environment variable is required but not set");
}

const pool = new pg.Pool({
  connectionString: DATABASE_URL,
  max: 10,
});

export const db = drizzle(pool, { schema });
export { schema };
