package com.mehrnet.bamdad

import android.app.AlertDialog
import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.graphics.Color
import android.graphics.drawable.Drawable
import android.text.Editable
import android.text.InputType
import android.text.TextUtils
import android.text.TextWatcher
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.BaseAdapter
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.PopupMenu
import android.widget.TextView
import java.util.Locale

/**
 * Split-tunneling app picker: search by display name or package, icons, and
 * several sort orders. The previous multi-choice AlertDialog showed only a
 * package label and could not be searched, which is unusable with a phone
 * full of apps.
 */
object SplitAppsDialog {
    private data class AppEntry(
        val packageName: String,
        val label: String,
        val system: Boolean,
        val icon: Drawable?,
    )

    private enum class Sort(val label: String) {
        NAME_ASC("Name A→Z"),
        NAME_DESC("Name Z→A"),
        PACKAGE_ASC("Package A→Z"),
        SYSTEM_FIRST("System first"),
        USER_FIRST("User first"),
        SELECTED_FIRST("Selected first"),
    }

    fun show(activity: MainActivity, current: String) {
        val density = activity.resources.displayMetrics.density
        fun dp(value: Int) = (value * density).toInt()

        val packageManager = activity.packageManager
        val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        val apps = packageManager.queryIntentActivities(launcher, 0)
            .mapNotNull { it.activityInfo?.applicationInfo }
            .distinctBy { it.packageName }
            .map { info ->
                AppEntry(
                    packageName = info.packageName,
                    label = packageManager.getApplicationLabel(info).toString(),
                    system = (info.flags and ApplicationInfo.FLAG_SYSTEM) != 0,
                    icon = runCatching { info.loadIcon(packageManager) }.getOrNull(),
                )
            }

        val selected = current
            .split(',')
            .map { it.trim() }
            .filter { it.isNotEmpty() }
            .toMutableSet()

        var query = ""
        var sort = Sort.NAME_ASC
        val visible = mutableListOf<AppEntry>()

        fun ordered(): List<AppEntry> {
            val needle = query.trim().lowercase(Locale.ROOT)
            val filtered = if (needle.isEmpty()) {
                apps
            } else {
                apps.filter {
                    it.label.lowercase(Locale.ROOT).contains(needle) ||
                        it.packageName.lowercase(Locale.ROOT).contains(needle)
                }
            }
            val comparator = when (sort) {
                Sort.NAME_ASC -> compareBy<AppEntry> { it.label.lowercase(Locale.ROOT) }
                Sort.NAME_DESC -> compareByDescending<AppEntry> { it.label.lowercase(Locale.ROOT) }
                Sort.PACKAGE_ASC -> compareBy { it.packageName }
                Sort.SYSTEM_FIRST -> compareByDescending<AppEntry> { it.system }
                    .thenBy { it.label.lowercase(Locale.ROOT) }
                Sort.USER_FIRST -> compareBy<AppEntry> { it.system }
                    .thenBy { it.label.lowercase(Locale.ROOT) }
                Sort.SELECTED_FIRST -> compareByDescending<AppEntry> { it.packageName in selected }
                    .thenBy { it.label.lowercase(Locale.ROOT) }
            }
            return filtered.sortedWith(comparator)
        }

        var adapter: BaseAdapter? = null
        var counter: TextView? = null
        fun refresh() {
            visible.clear()
            visible.addAll(ordered())
            adapter?.notifyDataSetChanged()
            counter?.text = "${selected.size} selected"
        }

        val layout = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(8), dp(16), 0)
        }

        val searchRow = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }
        val search = EditText(activity).apply {
            hint = "Search apps"
            setSingleLine(true)
            inputType = InputType.TYPE_CLASS_TEXT
        }
        search.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {
                query = s?.toString().orEmpty()
                refresh()
            }
            override fun afterTextChanged(s: Editable?) {}
        })
        searchRow.addView(search, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        val sortButton = Button(activity).apply { text = sort.label }
        sortButton.setOnClickListener {
            val popup = PopupMenu(activity, sortButton)
            Sort.values().forEachIndexed { index, option ->
                popup.menu.add(0, index, index, option.label)
            }
            popup.setOnMenuItemClickListener { item ->
                sort = Sort.values()[item.itemId]
                sortButton.text = sort.label
                refresh()
                true
            }
            popup.show()
        }
        searchRow.addView(sortButton)
        layout.addView(searchRow)

        val actions = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }
        val selectAll = Button(activity).apply { text = "Select all" }
        val clearAll = Button(activity).apply { text = "Clear" }
        val countView = TextView(activity).apply {
            text = "${selected.size} selected"
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
        }
        counter = countView
        actions.addView(selectAll)
        actions.addView(clearAll)
        actions.addView(countView, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        layout.addView(actions)

        val list = ListView(activity)
        val listHeight = (activity.resources.displayMetrics.heightPixels * 0.45)
            .toInt()
            .coerceIn(dp(200), dp(460))
        layout.addView(list, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, listHeight))

        adapter = object : BaseAdapter() {
            override fun getCount(): Int = visible.size
            override fun getItem(position: Int): Any = visible[position]
            override fun getItemId(position: Int): Long = visible[position].packageName.hashCode().toLong()

            override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
                val app = visible[position]
                val row = LinearLayout(activity).apply {
                    orientation = LinearLayout.HORIZONTAL
                    gravity = Gravity.CENTER_VERTICAL
                    setPadding(dp(4), dp(8), dp(4), dp(8))
                    isClickable = true
                    setOnClickListener {
                        if (!selected.add(app.packageName)) {
                            selected.remove(app.packageName)
                        }
                        refresh()
                    }
                }
                val icon = ImageView(activity)
                icon.setImageDrawable(app.icon)
                row.addView(icon, LinearLayout.LayoutParams(dp(38), dp(38)))

                val texts = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
                val name = TextView(activity).apply {
                    text = app.label
                    setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
                    setTextColor(themeColor(activity, android.R.attr.textColorPrimary, Color.WHITE))
                    maxLines = 1
                    ellipsize = TextUtils.TruncateAt.END
                }
                val packageName = TextView(activity).apply {
                    text = app.packageName
                    setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
                    setTextColor(themeColor(activity, android.R.attr.textColorSecondary, Color.GRAY))
                    maxLines = 1
                    ellipsize = TextUtils.TruncateAt.END
                }
                texts.addView(name)
                texts.addView(packageName)
                val textParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
                textParams.marginStart = dp(12)
                row.addView(texts, textParams)

                val check = CheckBox(activity).apply {
                    isChecked = app.packageName in selected
                    isClickable = false
                    isFocusable = false
                }
                row.addView(check)
                return row
            }
        }
        list.adapter = adapter

        selectAll.setOnClickListener {
            visible.forEach { selected.add(it.packageName) }
            refresh()
        }
        clearAll.setOnClickListener {
            visible.forEach { selected.remove(it.packageName) }
            refresh()
        }

        refresh()

        val dialog = AlertDialog.Builder(activity)
            .setTitle("Split tunneling apps")
            .setView(layout)
            .setPositiveButton("Save") { _, _ ->
                MainActivity.nativeSplitAppsSelected(
                    selected.sorted().joinToString(", "),
                )
            }
            .setNegativeButton("Cancel", null)
            .create()
        dialog.window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_HIDDEN)
        dialog.show()
    }

    private fun themeColor(context: Context, attr: Int, fallback: Int): Int {
        val value = TypedValue()
        if (!context.theme.resolveAttribute(attr, value, true)) {
            return fallback
        }
        return if (value.resourceId != 0) {
            context.resources.getColor(value.resourceId, context.theme)
        } else {
            value.data
        }
    }
}
