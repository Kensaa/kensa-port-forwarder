import { z } from 'zod';

export const portSchema = z.number().positive().max(65_535);
export const clientTypeSchema = z.enum(['sender', 'receiver']);
export type ClientType = z.infer<typeof clientTypeSchema>;

export const messagesSchema = z.discriminatedUnion('type', [
    z.object({
        type: z.literal('register'),
        ssh_key: z.string(),
        uuid: z.string(),
        auto_accept: z.boolean(),
        port_whitelist: portSchema.array(),
        port_blacklist: portSchema.array(),
        client_type: clientTypeSchema
    }),
    z.object({
        type: z.literal('connect_to_host'),
        target: z.string(),
        port: portSchema
    }),
    z.object({
        type: z.literal('connect_accept')
    }),
    z.object({
        type: z.literal('connect_deny')
    })
]);
