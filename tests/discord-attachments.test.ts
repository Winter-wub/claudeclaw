import { test, expect } from "bun:test";
import { pickFileAttachments } from "../src/commands/discord";

const att = (id: string, filename: string, content_type?: string, flags?: number) => ({
  id,
  filename,
  content_type,
  url: `https://cdn.discordapp.com/${id}`,
  proxy_url: `https://media.discordapp.net/${id}`,
  size: 100,
  flags,
});

test("hands non-image/voice/text files to Claude as files", () => {
  const files = pickFileAttachments([
    att("1", "report.pdf", "application/pdf"),
    att("2", "data.json", "application/json"),
    att("3", "sheet.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
    att("4", "archive.bin"),
  ]);
  expect(files.map((a) => a.id)).toEqual(["1", "2", "3", "4"]);
});

test("leaves the first image, voice clip and text file to their dedicated paths", () => {
  const files = pickFileAttachments([
    att("img", "photo.png", "image/png"),
    att("voice", "voice-message.ogg", "audio/ogg", 1 << 13),
    att("txt", "notes.md"),
    att("pdf", "report.pdf", "application/pdf"),
  ]);
  expect(files.map((a) => a.id)).toEqual(["pdf"]);
});

test("passes extra images and text files through as files instead of dropping them", () => {
  const files = pickFileAttachments([
    att("img1", "a.png", "image/png"),
    att("img2", "b.jpg", "image/jpeg"),
    att("txt1", "a.txt", "text/plain"),
    att("txt2", "b.csv", "text/csv"),
  ]);
  expect(files.map((a) => a.id)).toEqual(["img2", "txt2"]);
});

test("returns nothing when there are no attachments", () => {
  expect(pickFileAttachments([])).toEqual([]);
});
